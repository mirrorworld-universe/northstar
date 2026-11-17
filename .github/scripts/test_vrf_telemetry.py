import hashlib
import json
import os
import shlex
from pathlib import Path
from http.server import BaseHTTPRequestHandler, HTTPServer
import tempfile
import threading
import unittest
from unittest.mock import patch

import continuity_devnet
from continuity_rollout_state import ARTIFACTS, service_dropin
import vrf_telemetry as telemetry

REQUEST = 'Anq8aGArLLuULSbVNSBGb78uuaqiLe5wvQ3qaY3uvQW5'


def entry(message, at=100, cursor='s=abc;i=1'):
    return json.dumps({'MESSAGE': message, '__REALTIME_TIMESTAMP': str(at), '__CURSOR': cursor}).encode() + b'\n'


class RelayTelemetry(unittest.TestCase):
    def test_simulation_evidence_retains_only_public_fixed_fields(self):
        message = json.dumps({'event': 'simulation_rejected', 'request': REQUEST, 'slot': 123,
                              'expired': False, 'error': {'diagnostic': 'not-for-public-output'}})
        result = telemetry.classify_entries(entry(message), 90, 110)
        self.assertEqual(result['status'], 'observed')
        self.assertEqual(result['counts']['simulation_rejected'], 1)
        self.assertEqual(result['unique_requests'], 1)
        self.assertEqual(result['events'][0], {'event': 'simulation_rejected', 'request': REQUEST, 'slot': 123,
                                             'expired': False, 'simulation_error': True, 'at_us': 100})
        self.assertNotIn('diagnostic', json.dumps(result)); self.assertNotIn('not-for-public-output', json.dumps(result))
        self.assertFalse(result['raw_messages_included'])

    def test_only_exact_structured_native_events_are_accepted(self):
        valid = {'event': 'simulation_rejected', 'request': REQUEST, 'slot': 123, 'expired': False, 'error': None}
        messages = ['Program log: ' + json.dumps(valid), json.dumps({**valid, 'slot': True}),
                    json.dumps({**valid, 'expired': 1}), json.dumps({**valid, 'request': 'not-a-public-key'}),
                    json.dumps({**valid, 'unexpected': 'field'}), json.dumps({'event': 'signed', 'request': REQUEST}), 'private error text']
        result = telemetry.classify_entries(b''.join(entry(value) for value in messages), 90, 110)
        self.assertEqual(result['counts']['simulation_rejected'], 0); self.assertEqual(result['events'], [])

    def test_window_prefix_and_unique_request_counts_are_distinct_from_attempts(self):
        event = json.dumps({'event': 'simulation_rejected', 'request': REQUEST, 'slot': 123, 'expired': False, 'error': None})
        raw = entry(event, 89) + entry(event, 100) + entry(event, 105) + entry(event, 111)
        result = telemetry.classify_entries(raw, 90, 110, prefix='Anq8aG')
        self.assertEqual(result['counts']['simulation_rejected'], 2); self.assertEqual(result['unique_requests'], 1)
        self.assertEqual(telemetry.classify_entries(raw, 90, 110, prefix='3s5CG3')['events'], [])

    def test_output_budget_marks_coverage_incomplete_instead_of_silent_success(self):
        message = json.dumps({'event': 'simulation_unavailable', 'request': REQUEST})
        result = telemetry.classify_entries(entry(message) * 3, 90, 110, maximum_events=2)
        self.assertEqual(result['status'], 'limited'); self.assertEqual(result['limit'], 'events')
        self.assertFalse(result['coverage_complete'])

    def test_time_windows_are_explicit_utc_bounded_and_ordered(self):
        start, end = telemetry.parse_window('2026-11-17T01:44:00Z', '2026-11-17T01:59:00Z')
        self.assertEqual(end - start, 900_000_000)
        for left, right in [('2026-11-17T01:44:00', '2026-11-17T01:59:00Z'),
                            ('2026-11-18T01:44:00Z', '2026-11-17T01:59:00Z'),
                            ('2026-11-01T01:44:00Z', '2026-11-17T01:59:00Z')]:
            with self.subTest(left=left), self.assertRaises(ValueError): telemetry.parse_window(left, right)

    def test_private_snapshot_reads_are_bounded_regular_and_no_follow(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); source = root / 'state'; source.write_bytes(b'{}'); source.chmod(0o600)
            self.assertEqual(telemetry.read_bounded(source, 2, os.getuid()), b'{}')
            with self.assertRaises(ValueError): telemetry.read_bounded(source, 1, os.getuid())
            source.chmod(0o644)
            with self.assertRaises(ValueError): telemetry.read_bounded(source, 2, os.getuid())
            source.chmod(0o600); link = root / 'link'; link.symlink_to(source)
            with self.assertRaises((ValueError, OSError)): telemetry.read_bounded(link, 2, os.getuid())
            fifo = root / 'fifo'; os.mkfifo(fifo, 0o600)
            with self.assertRaises(ValueError): telemetry.read_bounded(fifo, 2, os.getuid())

    def test_unavailable_journal_never_reports_zero_verified_rejections(self):
        report = telemetry.unavailable_events('execution')
        self.assertEqual(report['status'], 'unavailable'); self.assertIsNone(report['counts'])
        self.assertFalse(report['coverage_complete']); self.assertFalse(report['raw_messages_included'])


class LoopbackRpc(unittest.TestCase):
    def setUp(self):
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args): pass
            def do_POST(self):
                self.rfile.read(int(self.headers['Content-Length']))
                raw = json.dumps(self.server.response).encode()
                self.send_response(200); self.send_header('Content-Length', str(len(raw))); self.end_headers(); self.wfile.write(raw)
        self.server = HTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True); self.thread.start()
        self.addCleanup(self.close)

    def close(self):
        self.server.shutdown(); self.server.server_close(); self.thread.join(2)
        self.assertFalse(self.thread.is_alive())

    def rpc(self, method, value):
        self.server.response = {'jsonrpc': '2.0', 'id': 1, 'result': value}
        return telemetry.rpc(self.server.server_address[1], method)

    def test_sync_error_is_classified_without_returning_provider_text(self):
        result = self.rpc('northstarSysGetSyncStatus', {'isSyncing': False, 'latestL1Slot': 123,
            'latestSyncedSlot': 123, 'checkpointError': 'untrusted private diagnostic'})
        self.assertTrue(result['value']['checkpoint_error_present'])
        self.assertNotIn('untrusted', json.dumps(result)); self.assertNotIn('checkpointError', result['value'])

    def test_slots_are_bounded_nonboolean_u64_without_number_coercion(self):
        self.assertEqual(self.rpc('getSlot', (1 << 64) - 1)['value'], (1 << 64) - 1)
        for value in (True, -1, 1 << 64, '123'):
            with self.subTest(value=value): self.assertEqual(self.rpc('getSlot', value), {'status': 'unavailable'})
        self.server.response = {'jsonrpc': '2.0', 'id': True, 'result': 'ok'}
        self.assertEqual(telemetry.rpc(self.server.server_address[1], 'getHealth'), {'status': 'unavailable'})


class FileBackedStderr(unittest.TestCase):
    def test_effective_file_stderr_is_not_mistaken_for_an_absent_journal(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as directory:
            path = Path(directory) / 'stderr.log'; path.write_text(json.dumps({'event': 'simulation_unavailable', 'request': REQUEST}) + '\n'); path.chmod(0o600)
            identity = path.stat(); proc = '/proc/123/fd/2'; real_stat = os.stat
            values = {'MainPID': '123', 'InvocationID': 'a' * 32, 'ExecStart': 'bound', 'NRestarts': '0', 'ActiveState': 'active',
                      'SubState': 'running', 'StandardOutput': 'null', 'StandardError': 'inherit', 'LogNamespace': '',
                      'MemoryCurrent': '1', 'CPUUsageNSec': '1', 'TasksCurrent': '1'}
            binding = {'validator_pid': 123, 'invocation_id': 'a' * 32, 'session': REQUEST,
                       'native_owner_uid': os.getuid(), 'native_owner_gid': os.getgid()}
            def source_stat(name, *args, **kwargs): return identity if str(name) == proc else real_stat(name, *args, **kwargs)
            def rpc(_port, method):
                return {'status': 'observed', 'value': REQUEST if method == 'getSessionPda' else '11111111111111111111111111111111' if method == 'getGenesisHash' else 'ok'}
            with patch.object(telemetry, 'properties', return_value=values), patch.object(telemetry, 'qualified_binding', return_value=(binding, os.getuid())), \
                 patch.object(telemetry, 'relay_status', return_value={'phase': 'running'}), patch.object(telemetry, 'rpc', side_effect=rpc), \
                 patch.object(telemetry.os, 'readlink', return_value=str(path)), patch.object(telemetry.os, 'stat', side_effect=source_stat):
                result = telemetry.sample('a' * 40, 'b' * 64, '2026-11-17T01:44:00Z', '2026-11-17T01:59:00Z')
            self.assertEqual(result['events']['status'], 'limited')
            self.assertEqual(result['events']['source'], 'effective-stderr-file')
            self.assertEqual(result['events']['counts']['simulation_unavailable'], 1)
            self.assertFalse(result['events']['coverage_complete'])
            self.assertNotIn('at_us', result['events']['events'][0])


class FileStreamCursor(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(); self.addCleanup(self.temporary.cleanup)
        self.path = Path(self.temporary.name) / 'stderr.log'; self.path.write_bytes(b'older line\n'); self.path.chmod(0o600)
        self.start = '2026-11-17T10:00:00.000000Z'; self.end = '2026-11-17T10:00:01.000000Z'

    def scan(self, start=None, end=None, cursor=None, initialize=False, invocation='a' * 32):
        descriptor = os.open(self.path, os.O_RDONLY | os.O_NOFOLLOW)
        try: return telemetry.file_events(descriptor, 'b' * 64, invocation, start or self.start, end or self.end, cursor=cursor, initialize=initialize)
        finally: os.close(descriptor)

    def append(self, raw):
        with self.path.open('ab') as stream: stream.write(raw)

    def event(self):
        return json.dumps({'event': 'simulation_rejected', 'request': REQUEST, 'slot': 123,
                           'expired': False, 'error': {'diagnostic': 'do-not-export'}}).encode() + b'\n'

    def test_initial_fence_and_incremental_events_do_not_invent_source_time(self):
        self.append(self.event()); initial = self.scan(initialize=True)
        self.assertEqual(initial['status'], 'initialized'); self.assertEqual(initial['events'], [])
        self.append(self.event())
        result = self.scan('2026-11-17T10:00:01.000001Z', '2026-11-17T10:00:02.000000Z', initial['next_stderr_cursor'])
        self.assertTrue(result['coverage_complete']); self.assertEqual(result['counts']['simulation_rejected'], 1)
        self.assertEqual(result['events'][0]['timestamp_source'], 'observed_offset_interval')
        self.assertEqual(result['events'][0]['at_us'], telemetry.parse_window('2026-11-17T10:00:01.000001Z', '2026-11-17T10:00:02.000000Z')[1])
        self.assertNotIn('do-not-export', json.dumps(result))

    def test_partial_append_is_revisited_without_retaining_raw_text_in_the_cursor(self):
        initial = self.scan(initialize=True); raw = self.event(); self.append(raw[:20])
        partial = self.scan('2026-11-17T10:00:01.000001Z', '2026-11-17T10:00:02.000000Z', initial['next_stderr_cursor'])
        self.assertEqual(partial['events'], []); self.assertEqual(partial['partial_line_bytes_pending'], 20)
        self.append(raw[20:])
        result = self.scan('2026-11-17T10:00:02.000001Z', '2026-11-17T10:00:03.000000Z', partial['next_stderr_cursor'])
        self.assertEqual(result['counts']['simulation_rejected'], 1)
        empty = self.scan('2026-11-17T10:00:03.000001Z', '2026-11-17T10:00:04.000000Z', result['next_stderr_cursor'])
        self.assertEqual(empty['events'], [])

    def test_truncation_replacement_restart_or_interval_gap_requires_resync(self):
        initial = self.scan(initialize=True)
        for invocation, start in [('c' * 32, '2026-11-17T10:00:01.000001Z'), ('a' * 32, '2026-11-17T10:00:01.000002Z')]:
            result = self.scan(start, '2026-11-17T10:00:02.000000Z', initial['next_stderr_cursor'], invocation=invocation)
            self.assertTrue(result['resync_required']); self.assertIsNone(result['counts'])
        self.path.write_bytes(b'')
        self.assertTrue(self.scan('2026-11-17T10:00:01.000001Z', '2026-11-17T10:00:02.000000Z', initial['next_stderr_cursor'])['resync_required'])
        replacement = self.path.with_name('replacement'); replacement.write_bytes(b'new line\n' * 2); os.replace(replacement, self.path)
        self.assertTrue(self.scan('2026-11-17T10:00:01.000001Z', '2026-11-17T10:00:02.000000Z', initial['next_stderr_cursor'])['resync_required'])

    def test_byte_budget_is_explicit_instead_of_silent_zero_coverage(self):
        initial = self.scan(initialize=True); self.append(b'ordinary line\n' * 100)
        with patch.object(telemetry, 'MAX_LOG_BYTES', 32):
            result = self.scan('2026-11-17T10:00:01.000001Z', '2026-11-17T10:00:02.000000Z', initial['next_stderr_cursor'])
        self.assertFalse(result['coverage_complete']); self.assertEqual(result['limit'], 'bytes')

    def test_microsecond_window_roundtrip_does_not_use_float_epoch_arithmetic(self):
        start, end = telemetry.parse_window('2026-11-17T11:33:06.953001Z', '2026-11-17T11:33:06.953002Z')
        self.assertEqual(end - start, 1); self.assertEqual(start % 1_000_000, 953001)

    def test_effective_stderr_does_not_follow_a_symlink_parent(self):
        with tempfile.TemporaryDirectory(dir=Path.home()) as directory:
            root = Path(directory); actual = root / 'actual'; actual.mkdir(mode=0o700)
            path = actual / 'stderr'; path.write_bytes(b'event\n'); path.chmod(0o600)
            (root / 'alias').symlink_to(actual, target_is_directory=True)
            source = path.stat(); real_stat = os.stat
            def observed(name, *args, **kwargs): return source if str(name) == '/proc/123/fd/2' else real_stat(name, *args, **kwargs)
            binding = {'validator_pid': 123, 'native_owner_uid': os.getuid(), 'native_owner_gid': os.getgid()}
            with patch.object(telemetry.os, 'stat', side_effect=observed), patch.object(telemetry.os, 'readlink', return_value=str(root / 'alias/stderr')), \
                 self.assertRaises((OSError, ValueError)):
                telemetry.effective_stderr(binding)


class QualifiedNativeBinding(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(); self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name); self.revision = 'a' * 40
        state = root / 'state'; namespace = state / 'upgrades' / self.revision
        control = namespace / 'control'; control.mkdir(parents=True, mode=0o700)
        for path in (state, state / 'upgrades', namespace): path.chmod(0o711)
        binary = namespace / 'runtime' / ARTIFACTS['validator']; binary.parent.mkdir(parents=True, mode=0o700)
        (namespace / 'runtime').chmod(0o700); binary.write_bytes(b'\x7fELF-fixture'); binary.chmod(0o500)
        profile = root / 'profile.json'
        self.profile = {'version': 1, 'context': 'b' * 64,
            'configuration': {'worker': {'coordinator': REQUEST, 'allowed_consumers': [REQUEST], 'fulfiller': REQUEST,
                'verifier': REQUEST, 'vrf_public_key': REQUEST, 'coordinator_programdata_sha256': 'e' * 64, 'verifier_programdata_sha256': 'f' * 64},
                              'signer': {'policy_sha256': 'c' * 64}}}
        self.save(profile, self.profile); self.profile_sha = hashlib.sha256(profile.read_bytes()).hexdigest()
        configuration = namespace / 'runtime/vrf-relay.json'
        configuration.write_bytes(json.dumps(self.profile['configuration'], sort_keys=True, separators=(',', ':')).encode() + b'\n'); configuration.chmod(0o400)
        environment = {'NORTHSTAR_CHECKPOINT_PLAN_DIR': str(root / 'checkpoints'), 'NORTHSTAR_UNSETTLED_STATE_DIR': str(root / 'unsettled')}
        arguments = [str(binary), '--vrf-relay-config', str(configuration)]
        self.plan = {'version': 1, 'kind': 'native-upgrade', 'source_revision': self.revision,
            'identity': {'session': REQUEST, 'portal': REQUEST, 'token_bridge': REQUEST, 'validator': REQUEST, 'context': 'b' * 64}, 'l1_rpc_profile': 'full', 'vrf_relay': self.profile,
            'desired': {'uid': os.getuid(), 'gid': os.getgid(), 'arguments': arguments, 'state_environment': environment, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest()},
            'artifacts': {'validator': hashlib.sha256(binary.read_bytes()).hexdigest()}}
        self.control = control; self.save(control / 'plan.json', self.plan)
        self.save(control / 'progress.json', {'version': 1, 'plan_sha256': telemetry.checksum(self.plan), 'phase': 'qualified'})
        self.report = {'result': 'passed', 'source_revision': self.revision, 'upgrade_plan_sha256': telemetry.checksum(self.plan),
            'session_replaced': False, 'journal_paths_preserved': True, 'protocol_controls_unchanged': True, 'vrf_relay_enabled': True}
        self.save(control / 'report.json', self.report)
        dropin = root / 'validator.conf'; dropin.write_text(service_dropin(arguments, *environment.values())); dropin.chmod(0o600)
        self.save(control / 'installed.json', {'sha256': hashlib.sha256(dropin.read_bytes()).hexdigest()})
        command = ['/usr/bin/env', *[key + '=' + value for key, value in environment.items()], *arguments]
        self.values = {'ExecStart': '{ path=/usr/bin/env ; argv[]=' + shlex.join(command) + ' ; ignore_errors=no; }',
            'ActiveState': 'active', 'SubState': 'running', 'Restart': 'no', 'NRestarts': '0', 'MainPID': '123', 'InvocationID': 'd' * 32}
        read = telemetry.read_bounded
        def owner_read(path, maximum, owner, private=True):
            if path == dropin: self.assertEqual(owner, 0); owner = os.getuid()
            return read(path, maximum, owner, private)
        for name, value in (('STATE', state), ('PROFILE', profile), ('DROPIN', dropin), ('read_bounded', owner_read)):
            context = patch.object(telemetry, name, value); context.start(); self.addCleanup(context.stop)

    def save(self, path, value):
        path.write_text(json.dumps(value)); path.chmod(0o600)

    def bind(self): return telemetry.qualified_binding(self.revision, self.profile_sha, self.values, full_hash=True)

    def test_native_constants_and_installed_artifact_match_real_controller(self):
        self.assertEqual(continuity_devnet.DROPIN.name, 'zzzz-northstar-continuity-v2.conf')
        binding, owner = self.bind()
        self.assertEqual(owner, os.getuid()); self.assertTrue(binding['validator_hash_checked'])
        self.assertEqual(binding['expected_validator_sha256'], self.plan['artifacts']['validator'])

    def test_changed_profile_installed_record_and_effective_arguments_are_rejected(self):
        for key, value in [('MainPID', '0'), ('NRestarts', '1'), ('ExecStart', '{ argv[]=/another/native ; }')]:
            with self.subTest(key=key), patch.dict(self.values, {key: value}), self.assertRaises(ValueError): self.bind()
        self.save(self.control / 'installed.json', {'sha256': '0' * 64})
        with self.assertRaises(ValueError): self.bind()

    def test_replaced_dropin_cannot_override_the_qualified_plan_arguments(self):
        dropin = telemetry.DROPIN
        altered = dropin.read_text()
        command = self.values['ExecStart'].replace(' ; ignore_errors', ' --rpc-port 1234 ; ignore_errors')
        lines = altered.splitlines()
        for index, line in enumerate(lines):
            if line.startswith('ExecStart=') and line != 'ExecStart=': lines[index] = line + ' --rpc-port 1234'
        dropin.write_text('\n'.join(lines) + '\n')
        self.save(self.control / 'installed.json', {'sha256': hashlib.sha256(dropin.read_bytes()).hexdigest()})
        with patch.dict(self.values, {'ExecStart': command}), self.assertRaises(ValueError): self.bind()

    def test_changed_runtime_configuration_is_rejected(self):
        configuration = Path(self.plan['desired']['arguments'][-1]); configuration.chmod(0o600); configuration.write_text('{}')
        with self.assertRaises(ValueError): self.bind()

    def test_qualified_private_controls_do_not_accept_booleans_or_traversable_controls(self):
        self.save(self.control / 'progress.json', {'version': True, 'plan_sha256': telemetry.checksum(self.plan), 'phase': 'qualified'})
        with self.assertRaises(ValueError): self.bind()
        self.save(self.control / 'progress.json', {'version': 1, 'plan_sha256': telemetry.checksum(self.plan), 'phase': 'qualified'})
        self.control.chmod(0o711)
        with self.assertRaises(ValueError): self.bind()

    def test_retained_state_claim_is_mandatory_and_metadata_not_a_qualification(self):
        self.report['journal_paths_preserved'] = False; self.save(self.control / 'report.json', self.report)
        with self.assertRaises(ValueError): self.bind()


if __name__ == '__main__': unittest.main()
