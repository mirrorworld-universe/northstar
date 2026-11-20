#!/usr/bin/env python3
"""Sonic: Bounded, read-only ER relay telemetry; never signing or service control."""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import shlex
import stat
import subprocess
import time
import urllib.request

STATE = Path('/solana/northstar/continuity-v2')
PROFILE = Path('/solana/northstar/vrf-isolated/profile.json')
RELAY = Path('/solana/northstar/vrf-isolated/relay-journal/native-relay-status.json')
DROPIN = Path('/etc/systemd/system/northstar-validator.service.d/zzzz-northstar-continuity-v2.conf')
SERVICE = 'northstar-validator.service'
ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
MAX_LOG_BYTES = 8 * 1024 * 1024
MAX_LOG_ENTRIES = 8192
MAX_EVENTS = 128
MAX_U64 = (1 << 64) - 1
FAILURE_CODES = {'stderr_namespace', 'stderr_file_deleted', 'stderr_file_identity', 'stderr_file_writable', 'stderr_inode_mismatch',
                 'stderr_changed_during_sample', 'qualified_native_record', 'native_binary_namespace', 'native_binary_digest',
                 'effective_native_command', 'installed_relay_configuration', 'public_binding_schema'}


def require(condition, reason):
    if not condition: raise ValueError(reason)


def checksum(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()).hexdigest()


def public_key(value):
    if not isinstance(value, str) or not 32 <= len(value) <= 44 or any(char not in ALPHABET for char in value): return False
    number = 0
    for char in value: number = number * 58 + ALPHABET.index(char)
    return len(value) - len(value.lstrip('1')) + (number.bit_length() + 7) // 8 == 32


def integer(value, maximum=MAX_U64):
    return type(value) is int and 0 <= value <= maximum


def read_bounded(path, maximum, owner, private=True):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_nlink == 1 and before.st_uid == owner
                and before.st_size <= maximum and not before.st_mode & (0o077 if private else 0o022), 'file_identity')
        raw = stream.read(maximum + 1); after = os.fstat(stream.fileno())
    require(len(raw) == before.st_size == after.st_size and before.st_mtime_ns == after.st_mtime_ns
            and before.st_ctime_ns == after.st_ctime_ns, 'file_changed')
    return raw


def directory(path, owner, private=False):
    value = path.lstat()
    require(stat.S_ISDIR(value.st_mode) and value.st_uid == owner
            and stat.S_IMODE(value.st_mode) in ((0o700,) if private else (0o700, 0o711)), 'directory_identity')


def parse_window(start, end):
    require(isinstance(start, str) and isinstance(end, str) and start.endswith('Z') and end.endswith('Z'), 'utc_window')
    left, right = (datetime.fromisoformat(value[:-1] + '+00:00') for value in (start, end))
    require(left.tzinfo == timezone.utc and right.tzinfo == timezone.utc and 0 < (right - left).total_seconds() <= 26 * 3600,
            'bounded_window')
    epoch = datetime(1970, 1, 1, tzinfo=timezone.utc)
    def micros(value):
        delta = value - epoch
        return (delta.days * 86400 + delta.seconds) * 1_000_000 + delta.microseconds
    return micros(left), micros(right)


def unavailable_events(reason):
    return {'version': 1, 'status': 'unavailable', 'coverage_complete': False, 'reason': reason,
            'counts': None, 'events': [], 'unique_requests': None, 'raw_messages_included': False}


def classify_entries(raw, start_us, end_us, prefix='', maximum_events=MAX_EVENTS):
    require(isinstance(raw, bytes) and len(raw) <= MAX_LOG_BYTES and integer(start_us) and integer(end_us)
            and start_us <= end_us and (not prefix or 6 <= len(prefix) <= 44 and all(char in ALPHABET for char in prefix))
            and type(maximum_events) is int and 1 <= maximum_events <= MAX_EVENTS, 'event_query_scope')
    result = {'version': 1, 'status': 'observed', 'coverage_complete': True, 'entries_examined': 0,
              'counts': {'simulation_rejected': 0, 'simulation_unavailable': 0}, 'events': [],
              'unique_requests': 0, 'raw_messages_included': False, 'scope': 'Retained journal observations, not proof of log-loss absence'}
    identities = set()
    for line in raw.splitlines():
        result['entries_examined'] += 1
        if result['entries_examined'] > MAX_LOG_ENTRIES:
            result.update(status='limited', coverage_complete=False, limit='entries'); break
        try:
            outer = json.loads(line)
            require(isinstance(outer, dict), 'journal_record')
            timestamp = outer.get('__REALTIME_TIMESTAMP')
            require(isinstance(timestamp, str) and re.fullmatch('[0-9]{1,20}', timestamp), 'journal_time')
            at = int(timestamp)
            if not start_us <= at <= end_us: continue
            message = outer.get('MESSAGE')
            if not isinstance(message, str) or not message.startswith('{') or len(message) > 64 * 1024: continue
            value = json.loads(message)
            if not isinstance(value, dict): continue
            event = value.get('event'); request = value.get('request')
            if not public_key(request) or not request.startswith(prefix): continue
            if event == 'simulation_rejected':
                if (set(value) != {'event', 'request', 'slot', 'expired', 'error'} or not integer(value['slot'])
                        or type(value['expired']) is not bool): continue
                selected = {'event': event, 'request': request, 'slot': value['slot'], 'expired': value['expired'],
                            'simulation_error': value['error'] is not None, 'at_us': at}
            elif event == 'simulation_unavailable' and set(value) == {'event', 'request'}:
                selected = {'event': event, 'request': request, 'at_us': at}
            else: continue
            if len(result['events']) == maximum_events:
                result.update(status='limited', coverage_complete=False, limit='events'); break
            result['events'].append(selected); result['counts'][event] += 1; identities.add(request)
            cursor = outer.get('__CURSOR')
            if isinstance(cursor, str) and re.fullmatch(r'[A-Za-z0-9_;:=+./-]{1,1024}', cursor): result['next_cursor'] = cursor
        except (ValueError, TypeError, KeyError):
            result.update(status='limited', coverage_complete=False, limit='malformed_record')
    result['unique_requests'] = len(identities)
    return result


def bounded_command(arguments, maximum, seconds):
    started = time.monotonic(); chunks = []; total = 0; limited = None
    with subprocess.Popen(arguments, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as process:
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while True:
                    remaining = seconds - (time.monotonic() - started)
                    if remaining <= 0: limited = 'time'; break
                    ready = selector.select(min(remaining, 0.25))
                    if not ready:
                        if process.poll() is not None: break
                        continue
                    chunk = os.read(process.stdout.fileno(), min(65536, maximum + 1 - total))
                    if not chunk: break
                    chunks.append(chunk); total += len(chunk)
                    if total > maximum: limited = 'bytes'; break
            if limited: process.kill()
            code = process.wait(timeout=2)
        except BaseException:
            process.kill(); process.wait(timeout=2); raise
    return code, b''.join(chunks)[:maximum], limited


def journal_events(start, end, prefix='', after_cursor=None, namespace=''):
    start_us, end_us = parse_window(start, end)
    arguments = ['journalctl', '--unit', SERVICE, '--no-pager', '--output=json',
                 '--output-fields=MESSAGE,__REALTIME_TIMESTAMP,__CURSOR', '--since', '@' + str(start_us // 1_000_000) + '.' + str(start_us % 1_000_000).zfill(6),
                 '--until', '@' + str(end_us // 1_000_000) + '.' + str(end_us % 1_000_000).zfill(6),
                 '--grep', '^\\{.*"event"[[:space:]]*:[[:space:]]*"simulation_(rejected|unavailable)"', '--lines', str(MAX_LOG_ENTRIES + 1)]
    if namespace:
        require(isinstance(namespace, str) and re.fullmatch('[A-Za-z0-9_.-]{1,64}', namespace), 'journal_namespace')
        arguments.append('--namespace=' + namespace)
    if after_cursor is not None:
        require(isinstance(after_cursor, str) and re.fullmatch(r'[A-Za-z0-9_;:=+./-]{1,1024}', after_cursor), 'journal_cursor')
        arguments += ['--after-cursor', after_cursor]
    try:
        code, raw, limit = bounded_command(arguments, MAX_LOG_BYTES, 10)
    except (OSError, subprocess.TimeoutExpired): return unavailable_events('execution')
    if code and limit is None: return unavailable_events('execution')
    result = classify_entries(raw, start_us, end_us, prefix)
    if limit is not None: result.update(status='limited', coverage_complete=False, limit=limit)
    return result


def effective_stderr(binding):
    proc = '/proc/' + str(binding['validator_pid']) + '/fd/2'; metadata = os.stat(proc)
    if not stat.S_ISREG(metadata.st_mode): return None
    path = Path(os.readlink(proc)); owner = binding['native_owner_uid']
    require(not str(path).endswith(' (deleted)'), 'stderr_file_deleted')
    require(path.is_absolute() and len(path.parts) > 1 and len(str(path)) <= 4096
            and path.parts[1] not in ('proc', 'sys', 'dev', 'etc', 'run')
            and all(part not in ('.', '..') and not any(ord(char) < 32 for char in part) for part in path.parts), 'stderr_namespace')
    parent = os.open('/', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for component in path.parts[1:-1]:
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
            os.close(parent); parent = child
        descriptor = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
    finally: os.close(parent)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_uid in (0, owner) and info.st_nlink == 1, 'stderr_file_identity')
        require(not info.st_mode & 0o022, 'stderr_file_writable')
        require((info.st_dev, info.st_ino) == (metadata.st_dev, metadata.st_ino), 'stderr_inode_mismatch')
        return descriptor, checksum({'path': str(path), 'device': info.st_dev, 'inode': info.st_ino})
    except BaseException:
        os.close(descriptor); raise


def file_events(descriptor, source, invocation, start, end, prefix='', cursor=None, initialize=False):
    start_us, end_us = parse_window(start, end); info = os.fstat(descriptor)
    result = {'version': 1, 'source': 'effective-stderr-file', 'status': 'observed', 'coverage_complete': True,
              'counts': {'simulation_rejected': 0, 'simulation_unavailable': 0}, 'events': [], 'unique_requests': 0,
              'raw_messages_included': False, 'timestamp_source': 'observed_offset_interval', 'requested_window_applied': False}
    historical = cursor is None and not initialize; line_start = True
    if cursor is not None:
        try:
            require(isinstance(cursor, str) and re.fullmatch('[A-Za-z0-9_-]{1,1024}', cursor), 'stderr_cursor')
            saved = json.loads(base64.urlsafe_b64decode(cursor + '=' * (-len(cursor) % 4)))
            require(isinstance(saved, dict) and set(saved) == {'version', 'source', 'invocation', 'device', 'inode', 'offset', 'until_us', 'line_start'}
                    and type(saved['version']) is int and saved['version'] == 1 and saved['source'] == source and saved['invocation'] == invocation
                    and integer(saved['device']) and saved['device'] == info.st_dev and integer(saved['inode']) and saved['inode'] == info.st_ino
                    and integer(saved['offset']) and saved['offset'] <= info.st_size and integer(saved['until_us'])
                    and saved['until_us'] + 1 == start_us and type(saved['line_start']) is bool, 'stderr_cursor_binding')
            offset = saved['offset']; line_start = saved['line_start']
        except (ValueError, TypeError, KeyError):
            return {**unavailable_events('stderr_cursor_resync'), 'resync_required': True, 'source': 'effective-stderr-file'}
    elif initialize:
        offset = info.st_size
        if offset:
            os.lseek(descriptor, offset - 1, os.SEEK_SET); line_start = os.read(descriptor, 1) == b'\n'
        result.update(status='initialized', scope='Initial EOF fence; no historical coverage asserted')
    else:
        offset = max(0, info.st_size - MAX_LOG_BYTES); line_start = offset == 0
        result.update(status='limited', coverage_complete=False, limit='source_timestamps_unavailable', timestamp_source='unavailable',
                      scope='Retained file tail only; UTC event time cannot be established')
    identities = set(); consumed = 0; started = time.monotonic()
    if not initialize:
        os.lseek(descriptor, offset, os.SEEK_SET); chunks = []; remaining = min(MAX_LOG_BYTES, info.st_size - offset)
        while remaining:
            if time.monotonic() - started > 5:
                result.update(status='limited', coverage_complete=False, limit='time'); break
            chunk = os.read(descriptor, min(65536, remaining))
            if not chunk:
                result.update(status='limited', coverage_complete=False, limit='short_read'); break
            chunks.append(chunk); remaining -= len(chunk)
        raw = b''.join(chunks)
        if info.st_size - offset > MAX_LOG_BYTES: result.update(status='limited', coverage_complete=False, limit='bytes')
        for number, line in enumerate(raw.splitlines(keepends=True)):
            if time.monotonic() - started > 5: result.update(status='limited', coverage_complete=False, limit='time'); break
            if not line.endswith(b'\n'): break
            if number >= 65536: result.update(status='limited', coverage_complete=False, limit='lines'); break
            if not line_start: consumed += len(line); line_start = True; continue
            if line.startswith(b'{'):
                if len(line) > 65536: result.update(status='limited', coverage_complete=False, limit='message')
                else:
                    wrapped = json.dumps({'MESSAGE': line.decode(errors='replace').rstrip('\r\n'), '__REALTIME_TIMESTAMP': str(end_us)}).encode() + b'\n'
                    selected = classify_entries(wrapped, start_us, end_us, prefix)
                    for event in selected['events']:
                        if len(result['events']) >= MAX_EVENTS:
                            result.update(status='limited', coverage_complete=False, limit='events'); break
                        if historical: event.pop('at_us', None)
                        else: event['timestamp_source'] = 'observed_offset_interval'
                        result['events'].append(event); result['counts'][event['event']] += 1; identities.add(event['request'])
                    if result.get('limit') == 'events': break
            consumed += len(line)
        offset += consumed
        result['partial_line_bytes_pending'] = len(raw) - consumed if result.get('limit') not in ('events', 'lines', 'bytes', 'time', 'short_read') else None
    final = os.fstat(descriptor)
    if (final.st_dev, final.st_ino) != (info.st_dev, info.st_ino) or final.st_size < info.st_size:
        return {**unavailable_events('stderr_changed_during_read'), 'resync_required': True, 'source': 'effective-stderr-file'}
    saved = {'version': 1, 'source': source, 'invocation': invocation, 'device': info.st_dev, 'inode': info.st_ino,
             'offset': offset, 'until_us': end_us, 'line_start': line_start}
    result['next_stderr_cursor'] = base64.urlsafe_b64encode(json.dumps(saved, separators=(',', ':')).encode()).decode().rstrip('=')
    result['unique_requests'] = len(identities)
    return result


def properties():
    names = ['ActiveState', 'SubState', 'MainPID', 'NRestarts', 'Restart', 'InvocationID',
             'MemoryCurrent', 'CPUUsageNSec', 'TasksCurrent', 'StandardOutput', 'StandardError', 'LogNamespace', 'ExecStart']
    code, raw, limited = bounded_command(['systemctl', 'show', '--property=' + ','.join(names), SERVICE], 64 * 1024, 10)
    require(code == 0 and limited is None, 'unit_query')
    result = dict(line.split('=', 1) for line in raw.decode().splitlines() if '=' in line)
    require(set(names) <= set(result), 'unit_schema')
    return result


def qualified_binding(revision, expected_profile, values, full_hash=False):
    require(isinstance(revision, str) and re.fullmatch('[a-f0-9]{40}', revision)
            and isinstance(expected_profile, str) and re.fullmatch('[a-f0-9]{64}', expected_profile), 'binding_selector')
    owner = STATE.lstat().st_uid
    require(owner > 0, 'native_owner')
    for path in (STATE, STATE / 'upgrades', STATE / 'upgrades' / revision): directory(path, owner)
    namespace = STATE / 'upgrades' / revision; control = namespace / 'control'; directory(control, owner, private=True)
    def record(name): return json.loads(read_bounded(control / (name + '.json'), 2 * 1024 * 1024, owner))
    plan = record('plan'); plan_sha = checksum(plan); progress = record('progress'); report = record('report'); installed = record('installed')
    require(all(isinstance(value, dict) for value in (plan, progress, report, installed))
            and all(isinstance(plan.get(key), dict) for key in ('desired', 'identity', 'artifacts')), 'qualified_record_schema')
    require(type(plan.get('version')) is int and plan['version'] == 1 and plan.get('kind') == 'native-upgrade'
            and plan.get('source_revision') == revision and type(progress.get('version')) is int
            and progress == {'version': 1, 'plan_sha256': plan_sha, 'phase': 'qualified'}
            and report.get('result') == 'passed' and report.get('source_revision') == revision
            and report.get('upgrade_plan_sha256') == plan_sha and report.get('session_replaced') is False
            and report.get('journal_paths_preserved') is True and report.get('protocol_controls_unchanged') is True
            and report.get('vrf_relay_enabled') is True and plan.get('l1_rpc_profile') == 'full'
            and type(plan['desired'].get('uid')) is int and plan['desired']['uid'] > 0
            and integer(plan['desired'].get('gid')), 'qualified_native_record')
    directory(PROFILE.parent, owner)
    raw_profile = read_bounded(PROFILE, 65536, owner)
    require(hashlib.sha256(raw_profile).hexdigest() == expected_profile, 'profile_digest')
    profile = json.loads(raw_profile)
    require(isinstance(profile, dict) and profile == plan.get('vrf_relay') and profile.get('context') == plan['identity']['context']
            and isinstance(profile.get('configuration'), dict)
            and all(isinstance(profile['configuration'].get(key), dict) for key in ('worker', 'signer')), 'retained_profile')
    raw_dropin = read_bounded(DROPIN, 65536, 0)
    require(set(installed) == {'sha256'} and hashlib.sha256(raw_dropin).hexdigest() == installed['sha256'], 'installed_dropin')
    expected_commands = [line.split('=', 1)[1] for line in raw_dropin.decode().splitlines() if line.startswith('ExecStart=') and line != 'ExecStart=']
    actual_commands = re.findall(r'argv\[\]=(.*?)(?= ; |;| \})', values.get('ExecStart', ''))
    actual_paths = re.findall(r'(?:^|[ {])path=([^;]+);', values.get('ExecStart', ''))
    environment = plan['desired']['state_environment']; arguments = plan['desired']['arguments']
    require(isinstance(environment, dict) and set(environment) == {'NORTHSTAR_CHECKPOINT_PLAN_DIR', 'NORTHSTAR_UNSETTLED_STATE_DIR'}
            and isinstance(arguments, list) and arguments and all(isinstance(argument, str) for argument in arguments), 'desired_native_command')
    desired_command = ['/usr/bin/env', *[key + '=' + environment[key] for key in ('NORTHSTAR_CHECKPOINT_PLAN_DIR', 'NORTHSTAR_UNSETTLED_STATE_DIR')], *arguments]
    require(len(expected_commands) == len(actual_commands) == len(actual_paths) == 1 and actual_paths[0].strip() == '/usr/bin/env'
            and shlex.split(actual_commands[0]) == shlex.split(expected_commands[0].replace('%%', '%').replace('$$', '$')) == desired_command,
            'effective_native_command')
    owner = plan['desired']['uid']
    for path in (namespace / 'runtime', namespace / 'runtime/bin'): directory(path, owner, private=True)
    configuration = namespace / 'runtime/vrf-relay.json'; selected = []
    for index, argument in enumerate(arguments):
        if argument == '--vrf-relay-config':
            require(index + 1 < len(arguments), 'relay_configuration_argument'); selected.append(arguments[index + 1])
        elif argument.startswith('--vrf-relay-config='): selected.append(argument.split('=', 1)[1])
    require(selected == [str(configuration)], 'managed_relay_configuration_argument')
    configuration_sha = hashlib.sha256(read_bounded(configuration, 65536, owner)).hexdigest()
    expected_configuration = json.dumps(profile['configuration'], sort_keys=True, separators=(',', ':'), allow_nan=False).encode() + b'\n'
    require(configuration_sha == hashlib.sha256(expected_configuration).hexdigest(), 'installed_relay_configuration')
    require(values.get('ActiveState') == 'active' and values.get('SubState') == 'running' and values.get('Restart') == 'no'
            and values.get('NRestarts') == '0' and re.fullmatch('[1-9][0-9]*', values.get('MainPID', ''))
            and re.fullmatch('[0-9a-f]{32}', values.get('InvocationID', '')),
            'native_lifecycle')
    binary = namespace / 'runtime/bin/agave-validator'
    metadata = binary.lstat()
    require(stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1 and metadata.st_uid == owner
            and stat.S_IMODE(metadata.st_mode) == 0o500 and plan['desired']['arguments'][0] == str(binary)
            and plan['desired']['binary_sha256'] == plan['artifacts']['validator'], 'native_binary_namespace')
    if full_hash:
        require(hashlib.sha256(read_bounded(binary, 1024 * 1024 * 1024, owner)).hexdigest() == plan['artifacts']['validator'], 'native_binary_digest')
    worker = profile['configuration']['worker']; signer = profile['configuration']['signer']
    require(isinstance(worker.get('allowed_consumers'), list) and len(worker['allowed_consumers']) == 1
            and all(public_key(value) for value in [worker.get(key) for key in ('coordinator', 'verifier', 'fulfiller', 'vrf_public_key')] + worker['allowed_consumers'])
            and all(public_key(plan['identity'].get(key)) for key in ('session', 'portal', 'token_bridge', 'validator'))
            and isinstance(plan['identity'].get('context'), str)
            and re.fullmatch('[a-f0-9]{64}', plan['identity']['context']), 'public_binding_schema')
    return {'native_revision': revision, 'plan_sha256': plan_sha, 'profile_sha256': expected_profile, 'session': plan['identity']['session'],
            'portal': plan['identity']['portal'], 'token_bridge': plan['identity']['token_bridge'], 'validator': plan['identity']['validator'],
            'context': plan['identity']['context'], 'native_owner_uid': owner, 'native_owner_gid': plan['desired']['gid'],
            'coordinator': worker['coordinator'], 'consumer': worker['allowed_consumers'][0],
            'fulfiller': worker['fulfiller'], 'verifier': worker['verifier'], 'vrf_public_key': worker['vrf_public_key'],
            'coordinator_programdata_sha256': worker['coordinator_programdata_sha256'],
            'verifier_programdata_sha256': worker['verifier_programdata_sha256'],
            'signer_policy_sha256': signer['policy_sha256'], 'validator_pid': int(values['MainPID']),
            'invocation_id': values['InvocationID'], 'runtime_configuration_sha256': configuration_sha,
            'expected_validator_sha256': plan['artifacts']['validator'],
            'validator_hash_checked': full_hash, 'binary_identity': [metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns, metadata.st_ctime_ns]}, owner


def relay_status(binding, owner):
    directory(RELAY.parent, owner, private=True)
    value = json.loads(read_bounded(RELAY, 16384, owner))
    fields = {'version', 'phase', 'proof_mode', 'session', 'session_context', 'fulfiller', 'coordinator', 'signer_policy_sha256', 'records', 'confirmed'}
    require(isinstance(value, dict) and set(value) in (fields, fields | {'simulation_rejected_requests'})
            and type(value['version']) is int and value['version'] == 1 and value['proof_mode'] == 'remote-only'
            and value['phase'] in ('running', 'blocked', 'waiting-for-continuity')
            and all(value[key] == binding[target] for key, target in (('session', 'session'), ('session_context', 'context'),
                ('fulfiller', 'fulfiller'), ('coordinator', 'coordinator'), ('signer_policy_sha256', 'signer_policy_sha256')))
            and integer(value['records'], 128) and integer(value['confirmed'], value['records']), 'relay_status')
    rejected = value.get('simulation_rejected_requests', [])
    require(isinstance(rejected, list) and len(rejected) <= 128 and len(set(rejected)) == len(rejected)
            and all(public_key(item) for item in rejected), 'relay_rejection_status')
    return {'phase': value['phase'], 'hot_records': value['records'], 'hot_confirmed': value['confirmed'],
            'simulation_rejected_requests': rejected, 'scope': 'Current tick sample, not historical rejection coverage'}


def rpc(port, method):
    payload = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': []}).encode()
    request = urllib.request.Request('http://127.0.0.1:' + str(port), data=payload, headers={'content-type': 'application/json'})
    try:
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=5) as response:
            raw = response.read(65537)
        require(len(raw) <= 65536, 'rpc_response_bound')
        value = json.loads(raw)
        require(isinstance(value, dict) and value.get('jsonrpc') == '2.0' and type(value.get('id')) is int
                and value['id'] == 1, 'rpc_envelope')
        if 'error' in value: return {'status': 'unavailable'}
        result = value['result']
        if method == 'northstarSysGetSyncStatus':
            require(isinstance(result, dict) and type(result.get('isSyncing')) is bool
                    and all(integer(result.get(key)) for key in ('latestL1Slot', 'latestSyncedSlot')), 'rpc_sync_schema')
            result = {key: result[key] for key in ('isSyncing', 'latestL1Slot', 'latestSyncedSlot')} | {
                'checkpoint_error_present': value['result'].get('checkpointError') is not None}
        elif method == 'getSlot': require(integer(result), 'rpc_slot_schema')
        elif method == 'getHealth': require(result == 'ok', 'rpc_health_schema')
        elif method in ('getGenesisHash', 'getSessionPda'): require(public_key(result), 'rpc_identity_schema')
        else: raise ValueError('rpc_method_scope')
        return {'status': 'observed', 'value': result}
    except (OSError, ValueError, KeyError, TimeoutError): return {'status': 'unavailable'}


def sample(revision, profile, start, end, prefix='', cursor=None, full_hash=False, stderr_cursor=None, initialize_stderr=False):
    before = properties(); binding, owner = qualified_binding(revision, profile, before, full_hash)
    relay = relay_status(binding, owner)
    rpc_values = {'l1_health': rpc(8899, 'getHealth'), 'er_health': rpc(8910, 'getHealth'),
                  'er_session': rpc(8910, 'getSessionPda'), 'er_genesis': rpc(8910, 'getGenesisHash'),
                  'er_slot': rpc(8910, 'getSlot'), 'sync': rpc(8910, 'northstarSysGetSyncStatus')}
    require(rpc_values['er_session'].get('value') in (None, binding['session'])
            and rpc_values['er_genesis'].get('value') in (None, '11111111111111111111111111111111'), 'rpc_identity')
    output_mode, error_mode = before['StandardOutput'], before['StandardError']
    journal = output_mode in ('journal', 'journal-or-kmsg') if error_mode == 'inherit' else error_mode in ('journal', 'journal-or-kmsg')
    source = effective_stderr(binding)
    if source is not None:
        descriptor, source_sha = source
        try:
            events = file_events(descriptor, source_sha, binding['invocation_id'], start, end, prefix, stderr_cursor, initialize_stderr)
            effective = os.stat('/proc/' + str(binding['validator_pid']) + '/fd/2'); held = os.fstat(descriptor)
            require((effective.st_dev, effective.st_ino) == (held.st_dev, held.st_ino), 'stderr_changed_during_sample')
        finally: os.close(descriptor)
        binding['stderr_source_sha256'] = source_sha
    else:
        events = journal_events(start, end, prefix, cursor, before['LogNamespace']) if journal else unavailable_events('stderr_not_journal')
        binding['stderr_source_sha256'] = checksum({'output': output_mode, 'error': error_mode, 'namespace': before['LogNamespace']})
    after = properties()
    require(all(before[key] == after[key] for key in ('MainPID', 'InvocationID', 'ExecStart', 'NRestarts', 'ActiveState', 'SubState')),
            'native_changed_during_sample')
    resources = {}
    for name in ('MemoryCurrent', 'CPUUsageNSec', 'TasksCurrent'):
        resources[name] = int(after[name]) if re.fullmatch('[0-9]{1,20}', after[name]) else None
    return {'version': 1, 'read_only': True, 'binding': binding, 'resources': resources, 'relay': relay, 'rpc': rpc_values,
            'events': events, 'native_service_actions': 0, 'signer_service_actions': 0, 'program_submissions': 0,
            'transactions_submitted': 0, 'scope': 'Telemetry only; not callback, settlement, or soak qualification'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native-revision', required=True); parser.add_argument('--profile-sha256', required=True)
    parser.add_argument('--since', required=True); parser.add_argument('--until', required=True)
    parser.add_argument('--request-prefix', default=''); parser.add_argument('--after-cursor'); parser.add_argument('--full-hash', action='store_true')
    parser.add_argument('--stderr-cursor'); parser.add_argument('--initialize-stderr', action='store_true')
    args = parser.parse_args()
    try:
        result = sample(args.native_revision, args.profile_sha256, args.since, args.until, args.request_prefix, args.after_cursor,
                        args.full_hash, args.stderr_cursor, args.initialize_stderr)
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError) as error:
        reason = str(error) if isinstance(error, ValueError) and str(error) in FAILURE_CODES else 'binding_or_namespace'
        print(json.dumps({'version': 1, 'read_only': True, 'status': 'unavailable', 'reason': reason,
                          'error_type': type(error).__name__, 'errno': error.errno if isinstance(error, OSError) else None,
                          'raw_messages_included': False}))
        return 1
    print(json.dumps(result, separators=(',', ':')))
    return 0


if __name__ == '__main__': raise SystemExit(main())
