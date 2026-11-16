"""Sonic: Read-only admission of a qualified retained ER-VRF handoff."""
import hashlib
from datetime import datetime,timezone
import json
import os
import stat
from pathlib import Path
import re
import shlex

from continuity_rollout_state import canonical, checked_directory, private_json, require, DEVNET
from continuity_devnet import LOADER, public_bytes

ROOT = Path('/solana/northstar/vrf-isolated/upgrade-receipts')
SIGNER = 'northstar-vrf-request-signer.service'
SIGNER_CONFIG = Path('/etc/northstar-vrf-request-signer/config.json')
SIGNER_UNIT = Path('/etc/systemd/system') / SIGNER
SIGNER_CURRENT = Path('/opt/northstar-vrf-request-signer/current')
ADMISSIONS = Path('/var/lib/private/northstar-vrf-request-signer/admissions/admissions.json')
LIMIT = 8_000_000_000
FIELDS = {
    'version','kind','result','source_revision','relay_source_revision','target_native_revision',
    'base_revision','base_plan_sha256','native_before','identity','er_genesis',
    'old_configuration_sha256','new_configuration_sha256','coordinator_before','coordinator_after',
    'coordinator_elf_sha256','coordinator_config','signer','relay_journal_sha256','retained_policy','peak_lamports',
    'native_service_actions','funding_transactions','program_submission_passes','signer_starts',
}

CHAIN_FIELDS = {'generation','parent_receipt_sha256','parent_profile_sha256','historical_policy_sha256'}
BARRIERS = ('artifacts','stop-attempted','frozen','upload-attempted','uploaded','configured','relay-frozen',
            'install-attempted','installed','start-attempted','ready')


def checksum(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def sha(value):
    return isinstance(value,str) and re.fullmatch('[0-9a-f]{64}',value) is not None


def revision(value):
    return isinstance(value,str) and re.fullmatch('[0-9a-f]{40}',value) is not None


def validate_receipt(value):
    require(isinstance(value,dict) and type(value.get('version')) is int and value['version'] in (1,2)
            and set(value)==(FIELDS|CHAIN_FIELDS if value['version']==2 else FIELDS)
            and value['kind']=='retained-er-vrf-handoff' and value['result']=='passed','retained VRF handoff schema differs')
    if value['version']==2:
        history=value['historical_policy_sha256'];generation=value['generation']
        require(type(generation) is int and 2<=generation<=17
                and all(sha(value[name]) and value[name]!='0'*64 for name in ('parent_receipt_sha256','parent_profile_sha256'))
                and isinstance(history,list) and len(history)==generation-1
                and all(sha(item) and item!='0'*64 for item in history) and len(set(history))==len(history),
                'retained handoff parent or bounded history differs')
    for name in ('source_revision','relay_source_revision','target_native_revision','base_revision'):
        require(revision(value[name]),'retained VRF source binding differs')
    for name in ('base_plan_sha256','old_configuration_sha256','new_configuration_sha256','coordinator_elf_sha256','relay_journal_sha256'):
        require(sha(value[name]),'retained VRF digest binding differs')
    from native_upgrade import STATE_KEYS
    native=value['native_before']
    require(isinstance(native,dict) and set(native)=={'arguments','binary_sha256','state_environment','uid','gid'}
            and isinstance(native['arguments'],list) and 1<=len(native['arguments'])<=256
            and all(isinstance(argument,str) and '\x00' not in argument for argument in native['arguments'])
            and Path(native['arguments'][0]).is_absolute() and sha(native['binary_sha256'])
            and type(native['uid']) is int and native['uid']>0 and type(native['gid']) is int and native['gid']>=0
            and isinstance(native['state_environment'],dict) and set(native['state_environment'])==set(STATE_KEYS)
            and all(isinstance(path,str) and '\x00' not in path and Path(path).is_absolute() for path in native['state_environment'].values()),
            'retained Native service specification differs')
    require(value['old_configuration_sha256']!=value['new_configuration_sha256'],'retained VRF handoff must change reviewed code bindings')
    for name,expected in (('native_service_actions',0),('funding_transactions',0),('program_submission_passes',1),('signer_starts',1)):
        require(type(value[name]) is int and value[name]==expected,'retained VRF operation count differs')
    require(type(value['peak_lamports']) is int and 0<value['peak_lamports']<=LIMIT,'retained VRF funding cap differs')
    for view in (value['coordinator_before'],value['coordinator_after']):
        require(isinstance(view,dict) and set(view)=={'programdata','deployment_slot','sha256'} and sha(view['sha256'])
                and isinstance(view['programdata'],str) and len(public_bytes(view['programdata']))==32
                and type(view['deployment_slot']) is int and 0<=view['deployment_slot']<1<<64,'retained Coordinator view differs')
    before,after=value['coordinator_before'],value['coordinator_after']
    require(before['programdata']==after['programdata'] and before['deployment_slot']<after['deployment_slot']
            and before['sha256']!=after['sha256'],'retained Coordinator identity or deployment progression differs')
    config=value['coordinator_config']
    require(isinstance(config,dict) and set(config)=={'address','sha256','default_deadline_slots','min_deadline_slots','max_deadline_slots','max_callback_compute'}
            and isinstance(config['address'],str) and len(public_bytes(config['address']))==32 and sha(config['sha256'])
            and all(type(config[name]) is int for name in ('default_deadline_slots','min_deadline_slots','max_deadline_slots','max_callback_compute'))
            and config['default_deadline_slots']==config['min_deadline_slots']==24_000 and config['max_deadline_slots']==65_535
            and 0<=config['max_callback_compute']<=1_200_000,'retained Coordinator deadline budget differs')
    retained=value['retained_policy']
    require(isinstance(retained,dict) and set(retained)=={'policy_sha256','coordinator_programdata_sha256'}
            and all(sha(digest) for digest in retained.values())
            and (retained['coordinator_programdata_sha256']==before['sha256'] if value['version']==1 else
                 retained['coordinator_programdata_sha256'] not in (before['sha256'],after['sha256'])
                 and retained['policy_sha256'] not in value['historical_policy_sha256']),
            'retained policy root differs')
    identity=value['identity']
    require(isinstance(identity,dict) and identity.get('version')==2 and identity.get('genesis')==DEVNET
            and sha(identity.get('context')) and isinstance(identity.get('session'),str)
            and value['er_genesis']=='11111111111111111111111111111111','retained VRF session identity differs')
    signer=value['signer']
    require(isinstance(signer,dict) and set(signer)=={'binary_sha256','configuration_sha256','unit_sha256','admissions_sha256','pid'}
            and type(signer['pid']) is int and signer['pid']>0
            and all(sha(signer[name]) for name in signer if name!='pid'),'retained signer binding differs')


def read_receipt(digest):
    require(sha(digest),'retained VRF receipt digest required')
    checked_directory(ROOT,private=True)
    value=private_json(ROOT/(digest+'.json'))
    require(checksum(value)==digest,'retained VRF receipt content differs')
    validate_receipt(value)
    return value


def validate_pid_reconciliation(review,original_plan):
    if 'native_pid_reconciliation' not in review:return None
    value=review['native_pid_reconciliation'];old=value['original_native'];current=value['current_native'];historical=review['diagnostic']
    require(value.get('version')==1 and value.get('kind')=='retained-er-native-pid-reconciliation'
            and value.get('source_revision')==review['source_revision'] and value.get('original_plan_sha256')==checksum(original_plan)
            and old==original_plan['review']['native'] and value.get('changed_native_fields')==['pid']
            and type(value.get('native_service_actions')) is int and value['native_service_actions']==0
            and value.get('maintenance_policy_changed') is False,'Native PID reconciliation scope differs')
    require(isinstance(old,dict) and isinstance(current,dict) and set(old)==set(current)=={'revision','plan_sha256','identity','pid'}
            and type(old['pid']) is int and old['pid']==1386853 and type(current['pid']) is int and current['pid']==2236901
            and revision(old['revision']) and sha(old['plan_sha256']) and isinstance(old['identity'],dict)
            and {key:item for key,item in old.items() if key!='pid'}=={key:item for key,item in current.items() if key!='pid'},
            'Native reconciliation changed more than the reviewed PID')
    actual=value['current_diagnostic']
    require(value.get('historical_diagnostic_sha256')==checksum(historical) and value.get('current_diagnostic_sha256')==checksum(actual)
            and actual.get('source_revision')==review['source_revision'] and actual.get('native_unchanged') is False
            and actual.get('native_binding')==current and actual.get('native_original_binding')==old,
            'Native reconciliation must retain truthful before/after diagnostics')
    retained=dict(actual);retained.pop('native_binding');retained.pop('native_original_binding')
    retained.update(source_revision=historical['source_revision'],native_unchanged=True)
    require(retained==historical,'retained state differs outside the reviewed Native PID')
    proof=value['restart_evidence'];process=proof.get('systemd',{});boot=proof.get('boot',{}).get('id','')
    require(value.get('restart_evidence_sha256')==checksum(proof) and proof.get('version')==1
            and proof.get('kind')=='native-restart-evidence' and proof.get('read_only') is True
            and proof.get('service')=='northstar-validator.service' and proof.get('raw_messages_included') is False
            and proof.get('baseline_refresh_allowed') is False and proof.get('continuation_allowed') is False
            and process.get('MainPID')==str(current['pid']) and process.get('ActiveState')=='active' and process.get('SubState')=='running'
            and process.get('Restart')=='no' and process.get('NRestarts')=='0' and process.get('UnitFileState')=='enabled'
            and process.get('ExecMainStartTimestamp')=='Sat 2026-11-14 06:18:55 UTC'
            and isinstance(process.get('InvocationID'),str) and re.fullmatch('[0-9a-f]{32}',process['InvocationID'])
            and isinstance(boot,str) and re.fullmatch('[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',boot),
            'Native maintenance restart process evidence differs')
    lifecycle=proof['journals']['unit'];events=lifecycle.get('events',[]);kernel=proof['journals']['kernel']
    require(lifecycle.get('status')=='classified' and lifecycle.get('entry_limit_reached') is False and lifecycle.get('event_output_limited') is False
            and [event.get('event') for event in events]==['stopping','deactivated','stopped','starting','started']
            and all(event.get('boot_id')==boot.replace('-','') for event in events)
            and kernel.get('status')=='classified' and kernel.get('entry_limit_reached') is False and kernel.get('marker_counts')=={},
            'Native maintenance lifecycle coverage differs')
    times=[datetime.fromisoformat(event['time']) for event in events]
    require(times==sorted(times) and times[-1].replace(microsecond=0)==datetime(2026,11,14,6,18,55,tzinfo=timezone.utc),
            'Native maintenance lifecycle time differs')
    package=proof['package_history']['unattended_dpkg']
    require(package.get('status')=='classified' and package.get('tail_limited') is False and package.get('record_output_limited') is False,
            'Native package maintenance evidence coverage differs')
    matching=[entry for entry in package.get('records',[]) if entry.get('needrestart_reference') is True
        and entry.get('native_unit_reference') is True and entry.get('native_restart_command_reference') is True and 'libc6' in entry.get('package_families',[])
        and datetime.fromisoformat(entry['started']).replace(tzinfo=timezone.utc)<=times[0]
        and datetime.fromisoformat(entry['finished']).replace(tzinfo=timezone.utc)>=times[-1].replace(microsecond=0)]
    require(len(matching)==1,'Native maintenance record does not bind the explicit restart')
    return checksum(value)


def verify_continuation(control,plan_sha,evidence,value,profile,original_plan):
    failure_path=control/'failure.json';root=control/'continuation';pointer=control/'continuation.json'
    if not os.path.lexists(failure_path):
        require(not os.path.lexists(root) and not os.path.lexists(pointer),'unexpected continuation without failed handoff')
        return
    failure=private_json(failure_path)
    require(failure.get('version')==1 and failure.get('plan_sha256')==plan_sha
            and failure.get('evidence',{}).get('phase')=='uploaded' and failure['evidence'].get('failure_type')=='ValueError'
            and failure['evidence'].get('signer_quarantine_verified') is True
            and failure['evidence'].get('automatic_rollback') is False,'failed retained handoff profile differs')
    checked_directory(root,private=True)
    plan=private_json(root/'plan.json');sha_plan=checksum(plan);review=plan['review']
    require(plan.get('version')==1 and plan.get('kind')=='retained-er-vrf-post-upload-continuation'
            and plan.get('review_sha256')==checksum(review) and review.get('version')==1
            and review.get('kind')=='retained-er-vrf-post-upload-continuation-review'
            and revision(review.get('source_revision')) and review.get('failed_source_revision')==value['source_revision']
            and review.get('original_plan_sha256')==plan_sha and review.get('native_revision')==value['target_native_revision'],
            'continued retained plan binding differs')
    require(private_json(pointer)=={'version':1,'source_revision':review['source_revision'],'plan_sha256':sha_plan}
            and private_json(root/'progress.json')=={'version':1,'plan_sha256':sha_plan,'phase':'qualified'},'continued handoff is not qualified')
    require(review.get('managed_control_abi')=='devnet-control' and sha(review.get('managed_control_sha256'))
            and type(review.get('remaining_signer_starts')) is int and review['remaining_signer_starts']==1
            and all(type(review.get(name)) is int and review[name]==0 for name in ('additional_program_submission_passes','additional_funding_transactions')),
            'continued handoff operation scope differs')
    diagnostic=review['diagnostic'];markers=diagnostic.get('log',{}).get('markers',{})
    require(revision(review.get('diagnostic_source_revision')) and checksum(diagnostic)==review.get('diagnostic_review_sha256')
            and diagnostic.get('source_revision')==review['diagnostic_source_revision'] and diagnostic.get('result')=='passed'
            and diagnostic.get('failed_source_revision')==value['source_revision'] and diagnostic.get('plan_sha256')==plan_sha
            and diagnostic.get('coordinator')==value['coordinator_after']
            and diagnostic.get('release_allowed') is False and diagnostic.get('retry_allowed') is False
            and diagnostic.get('signer')=={'active_state':'inactive','pid':0,'boot':'disabled','original_binding_unchanged':True}
            and all(diagnostic.get(name) is True for name in ('native_unchanged','admissions_unchanged','coordinator_configuration_unchanged',
                'deadline_intent_absent','deadline_started_marker_absent','unrelated_configuration_unchanged'))
            and markers.get('required_arguments')==1 and markers.get('er_control_usage',0)+markers.get('managed_control_usage',0)==1
            and markers.get('required_session_context',0)>=1 and markers.get('required_session',0)>=1
            and all(markers.get(name)==0 for name in ('rpc_send_failure','signed_event','signed_event_spaced','receipt_event','receipt_event_spaced')),
            'continued handoff unsigned diagnostic differs')
    continued={}
    for name in ('authorized','control','configuration-attempted','configured','relay-frozen','install-attempted','installed','start-attempted','ready'):
        marker=private_json(root/(name+'.json'))
        require(marker.get('version')==1 and marker.get('plan_sha256')==sha_plan and isinstance(marker.get('evidence'),dict),
                'continued handoff barrier binding differs')
        continued[name]=marker['evidence']
    reconciled=validate_pid_reconciliation(review,original_plan)
    authorization={'review_sha256':plan['review_sha256'],'diagnostic_review_sha256':review['diagnostic_review_sha256'],
        'additional_program_submission_passes':0,'additional_funding_transactions':0,'remaining_signer_starts':1}
    if reconciled is not None:authorization['native_pid_reconciliation_sha256']=reconciled
    require(continued['authorized']==authorization
            and continued['control']=={'sha256':review['managed_control_sha256'],'abi':'devnet-control'}
            and review['managed_control_sha256']!=evidence['artifacts'].get('sha256',{}).get('control')
            and all(continued[name]==evidence[name] for name in ('configured','relay-frozen','install-attempted','installed','start-attempted','ready')),
            'continued handoff artifact or remaining-step evidence differs')
    descriptor=os.open(root/'control',os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(descriptor,'rb') as stream:
        metadata=os.fstat(stream.fileno())
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_nlink==1 and metadata.st_uid==os.getuid()
                and not metadata.st_mode&0o022 and 0<metadata.st_size<=128*1024*1024,'continued managed control file differs')
        raw=stream.read(128*1024*1024+1);after=os.fstat(stream.fileno())
    require(raw.startswith(b'\x7fELF') and len(raw)==metadata.st_size and after.st_mtime_ns==metadata.st_mtime_ns
            and hashlib.sha256(raw).hexdigest()==review['managed_control_sha256'],'continued managed control artifact changed')
    report=private_json(root/'report.json')
    require(report.get('version')==1 and report.get('kind')=='retained-er-vrf-post-upload-continuation-report'
            and report.get('result')=='passed' and report.get('plan_sha256')==sha_plan and report.get('original_plan_sha256')==plan_sha
            and report.get('source_revision')==review['source_revision'] and report.get('failed_source_revision')==value['source_revision']
            and report.get('diagnostic_review_sha256')==review['diagnostic_review_sha256']
            and type(report.get('signer_starts')) is int and report['signer_starts']==1
            and all(type(report.get(name)) is int and report[name]==0 for name in ('additional_program_submission_passes','additional_funding_transactions','native_service_actions'))
            and report.get('handoff_receipt_sha256')==profile['retained_upgrade_receipt_sha256'] and report.get('callback_qualified') is False,
            'continued handoff qualified report differs')
    if reconciled is not None:
        require(private_json(root/'native-reconciled.json')=={'version':1,'plan_sha256':sha_plan,'evidence':review['native_pid_reconciliation']}
                and report.get('native_pid_reconciliation_sha256')==reconciled,'qualified Native reconciliation barrier differs')
    else:require(not os.path.lexists(root/'native-reconciled.json') and 'native_pid_reconciliation_sha256' not in report,
            'unreviewed Native reconciliation evidence is present')


def verify_controls(value,profile,ancestry=()):
    validate_receipt(value)
    current_sha=checksum(value)
    require(current_sha not in ancestry and len(ancestry)<17,'retained handoff ancestry is cyclic or exceeds bounds')
    state=ROOT.parent
    chained=value['version']==2
    if chained:
        state=state/'retained-upgrade-rounds'/str(value['generation'])
        checked_directory(state.parent,private=True);checked_directory(state,private=True)
    control=state/'retained-upgrades'/value['source_revision']
    checked_directory(control,private=True)
    plan=private_json(control/'plan.json');plan_sha=checksum(plan)
    require(isinstance(plan,dict) and set(plan)=={'version','kind','review_sha256','review'}
            and plan['version']==1 and plan['kind']=='retained-er-vrf-upgrade'
            and checksum(plan['review'])==plan['review_sha256'],'retained handoff plan binding differs')
    progress=private_json(control/'progress.json')
    require(progress in ({'version':1,'plan_sha256':plan_sha,'phase':'handoff-qualified'},
                         {'version':1,'plan_sha256':plan_sha,'phase':'callback-qualified'})
            and private_json(state/'retained-upgrade.json')=={'version':1,'plan_sha256':plan_sha,'source_revision':value['source_revision']},
            'retained handoff progress is not qualified')
    review=plan['review'];worker=profile['configuration']['worker']
    require(all(review.get(name)==value[name] for name in ('source_revision','target_native_revision','relay_source_revision'))
            and review.get('context')==value['identity']['context'] and review.get('program_ids')=={
                'coordinator':worker['coordinator'],'consumer':worker['allowed_consumers'][0],
                'verifier':worker['verifier'],'portal':value['identity']['portal']}
            and review.get('quote',{}).get('peak_lamports')==value['peak_lamports']
            and review.get('quote',{}).get('funding_transactions')==0,'retained handoff plan scope differs')
    evidence={};previous_sha=plan_sha
    for sequence,name in enumerate(BARRIERS,1):
        marker=private_json(control/(name+'.json'))
        fields={'version','plan_sha256','evidence'}|({'sequence','previous_sha256'} if chained else set())
        require(isinstance(marker,dict) and set(marker)==fields and marker['version']==(2 if chained else 1)
                and marker['plan_sha256']==plan_sha and isinstance(marker['evidence'],dict),'retained handoff durable barrier differs')
        if chained:
            require(type(marker['sequence']) is int and marker['sequence']==sequence
                    and marker['previous_sha256']==previous_sha,'retained handoff barrier hash chain differs')
            previous_sha=checksum(marker)
        evidence[name]=marker['evidence']
    require(evidence['uploaded'].get('view')==value['coordinator_after']
            and evidence['configured'].get('configuration')==value['coordinator_config']
            and evidence['relay-frozen'].get('state_sha256')==value['relay_journal_sha256']
            and evidence['artifacts'].get('sha256',{}).get('signer')==value['signer']['binary_sha256']
            and evidence['artifacts'].get('original_policy_sha256')==value['retained_policy']['policy_sha256'],
            'retained handoff program or frozen artifact evidence differs')
    if chained:
        require(evidence['artifacts'].get('sha256',{}).get('coordinator')==value['coordinator_elf_sha256']
                ==review.get('target_program_manifest',{}).get('programs',{}).get('vrf_coordinator.so')
                and review.get('target_program_manifest',{}).get('coordinator_features')==['er'],
                'chained handoff lacks the frozen explicit-ER Coordinator artifact binding')
    installed=evidence['installed'];ready=evidence['ready']
    require(installed.get('source_revision')==value['source_revision']
            and all(installed.get(name)==value['signer'][name] for name in ('binary_sha256','configuration_sha256','unit_sha256'))
            and ready.get('pid')==value['signer']['pid'] and ready.get('admissions_sha256')==value['signer']['admissions_sha256']
            and ready.get('info',{}).get('policy_sha256')==profile['configuration']['signer']['policy_sha256']
            and ready.get('info',{}).get('public_key')==worker['vrf_public_key']
            and evidence['frozen'].get('audit',{}).get('validation',{}).get('input_snapshot_sha256')==value['signer']['admissions_sha256'],
            'retained signer start or admission freeze evidence differs')
    report=private_json(control/'handoff-report.json')
    require(report.get('result')=='passed' and report.get('receipt_sha256')==profile['retained_upgrade_receipt_sha256']
            and type(report.get('native_service_actions')) is int and report['native_service_actions']==0
            and report.get('callback_qualified') is False,'retained handoff report differs')
    if chained:
        require(not os.path.lexists(control/'failure.json') and not os.path.lexists(control/'continuation.json'),
                'failed chained handoff requires separate operator reconciliation')
        verify_chain_parent(value,profile,control,plan,(*ancestry,current_sha))
    else:
        verify_continuation(control,plan_sha,evidence,value,profile,plan)
    return {'plan':plan,'plan_sha256':plan_sha,'evidence':evidence,'report':report,'control':control}


def verify_chain_parent(value,profile,control,plan,ancestry):
    from vrf_relay_profile import configuration_hash,retained_transition
    chain=plan['review'].get('handoff_chain')
    fields={'generation','parent_generation','parent_receipt_sha256','parent_profile_sha256','parent_plan_sha256','parent_profile'}
    require(isinstance(chain,dict) and set(chain)==fields and chain['generation']==value['generation']
            and type(chain['parent_generation']) is int and chain['parent_generation']==value['generation']-1
            and chain['parent_receipt_sha256']==value['parent_receipt_sha256']
            and chain['parent_profile_sha256']==value['parent_profile_sha256']
            and sha(chain['parent_plan_sha256']),'retained handoff parent plan binding differs')
    old=private_json(control/'old-profile.json')
    require(old==chain['parent_profile'] and checksum(old)==value['parent_profile_sha256']
            and old.get('retained_upgrade_receipt_sha256')==value['parent_receipt_sha256'],
            'retained handoff staged parent profile differs')
    parent=read_receipt(value['parent_receipt_sha256'])
    require(parent.get('generation',1)==chain['parent_generation'] and parent['identity']==value['identity']
            and parent['er_genesis']==value['er_genesis'] and parent['retained_policy']==value['retained_policy']
            and parent['coordinator_after']==value['coordinator_before']
            and parent['new_configuration_sha256']==value['old_configuration_sha256']==configuration_hash(old)
            and configuration_hash(profile)==value['new_configuration_sha256']
            and profile['configuration']['signer'].get('retained_policy')==value['retained_policy']
            and profile['configuration']['signer'].get('historical_policy_sha256')==value['historical_policy_sha256'],
            'retained handoff changed origin, context, or prior code binding')
    require(retained_transition(old,profile) is False,'retained handoff does not extend the exact prior policy')
    checked=verify_controls(parent,old,ancestry)
    require(checked['plan_sha256']==chain['parent_plan_sha256'],'qualified parent control plan differs')
    qualification=qualified_parent(parent,old,checked,plan['review'].get('original_profile_sha256'))
    require(checksum(qualification)==plan['review'].get('parent_callback_report_sha256'),
            'qualified parent callback evidence changed')


def qualified_parent(value,profile,checked,profile_sha256):
    require(sha(profile_sha256),'qualified parent profile artifact digest required')
    control=checked['control'];plan_sha=checked['plan_sha256']
    require(private_json(control/'progress.json')=={'version':1,'plan_sha256':plan_sha,'phase':'callback-qualified'},
            'parent callback qualification is absent')
    report=private_json(control/'callback-report.json')
    require(report.get('result')=='passed' and report.get('plan_sha256')==plan_sha
            and report.get('profile_sha256')==profile_sha256
            and report.get('receipt_sha256')==profile.get('retained_upgrade_receipt_sha256')
            and report.get('context')==value['identity']['context'] and report.get('session')==value['identity']['session']
            and report.get('program_ids')==checked['plan']['review']['program_ids']
            and all(report.get(name) is True for name in ('fresh_pda','request_closed','verifier_cpi','callback_output_matches'))
            and all(type(report.get(name)) is int and report[name]==0 for name in ('native_service_actions','program_submission_passes','funding_transactions')),
            'parent fresh-callback qualification does not bind retained state')
    return report


def frozen_coordinator(control,expected):
    descriptor=os.open(control/'artifacts/coordinator',os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(descriptor,'rb') as stream:
        metadata=os.fstat(stream.fileno())
        require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid==os.getuid()
                and stat.S_IMODE(metadata.st_mode)==0o600 and 0<metadata.st_size<=2*1024*1024,
                'frozen Coordinator artifact namespace differs')
        raw=stream.read(2*1024*1024+1)
    require(len(raw)==metadata.st_size and raw.startswith(b'\x7fELF')
            and hashlib.sha256(raw).hexdigest()==expected,'frozen Coordinator artifact content differs')
    return raw


def verify_native_base(upgrade,value):
    if value['base_revision']==upgrade.base.name:
        require(value['base_plan_sha256']==checksum(upgrade.base_plan),'retained VRF activation base differs')
        return
    from native_upgrade import STATE_KEYS,start_floor
    from continuity_rollout_state import service_dropin
    require(value['version']==2,'legacy retained VRF receipt cannot replace its activation base')
    previous=upgrade.parent/value['base_revision'];checked_directory(previous)
    control=previous/'control';checked_directory(control,private=True)
    plan=private_json(control/'plan.json');sha256=checksum(plan)
    require(plan.get('version')==1 and plan.get('kind')=='native-upgrade'
            and plan.get('source_revision')==previous.name and plan.get('base_revision')==upgrade.base.name
            and plan.get('base_plan_sha256')==checksum(upgrade.base_plan) and value['base_plan_sha256']==sha256
            and plan.get('desired')==value['native_before'] and plan.get('identity')==value['identity']
            and type(plan.get('l1_floor')) is int and plan['l1_floor']>=0,
            'chained VRF qualified Native base or retained activation lineage differs')
    require(private_json(control/'progress.json')=={'version':1,'plan_sha256':sha256,'phase':'qualified'}
            and private_json(control/'frozen.json').get('plan_sha256')==sha256,
            'chained VRF Native base lacks qualified freeze evidence')
    start_floor(control/'start-attempted.json',plan)
    desired=plan['desired'];text=service_dropin(desired['arguments'],*[desired['state_environment'][key] for key in STATE_KEYS])
    require(private_json(control/'installed.json')=={'sha256':hashlib.sha256(text.encode()).hexdigest()},
            'chained VRF Native base installation differs')
    report=private_json(control/'report.json')
    require(report.get('result')=='passed' and report.get('upgrade_plan_sha256')==sha256,
            'chained VRF Native base qualification differs')


def admit(upgrade,profile,previous,before,carried):
    from native_upgrade import service_spec
    from vrf_relay_profile import JOURNAL, configuration_hash
    value=read_receipt(profile['retained_upgrade_receipt_sha256'])
    checked=verify_controls(value,profile)
    verify_native_base(upgrade,value)
    worker=profile['configuration']['worker'];signer=profile['configuration']['signer']
    require(upgrade.recovery_slot is None and value['identity']['context']==profile['context']
            and value['identity']['session']==worker['er_session']
            and value['new_configuration_sha256']==configuration_hash(profile)
            and value['coordinator_after']['sha256']==worker['coordinator_programdata_sha256']
            and value['retained_policy']==signer['retained_policy'],'retained VRF receipt does not bind this upgrade')
    require(all(value['identity'].get(name)==upgrade.base_plan['programs'][name] for name in ('portal','token_bridge'))
            and value['identity'].get('validator')==upgrade.base_plan['validator']
            and value['identity'].get('profile')==upgrade.base_plan['profile'],'retained VRF context changed')
    if not carried:
        require(value['native_before']==service_spec(before) and value['old_configuration_sha256']==configuration_hash(previous)
                and value['coordinator_before']['sha256']==previous['configuration']['worker']['coordinator_programdata_sha256'],
                'retained VRF original service or policy differs')
    source_lock=json.loads((Path(__file__).resolve().parents[2]/'northstar/vrf-relay/source-lock.json').read_text())
    require(value['relay_source_revision']==source_lock['revision'],'retained VRF relay source differs')
    upgrade.ops.execute(['git','merge-base','--is-ancestor',value['target_native_revision'],upgrade.args.revision])
    program=upgrade.ops.upstream.account(worker['coordinator'])
    view=value['coordinator_after']
    require(program is not None and program['owner']==LOADER and program['executable'] and len(program['raw'])==36
            and program['raw'][:4]==(2).to_bytes(4,'little') and program['raw'][4:]==public_bytes(view['programdata']),
            'retained Coordinator program binding differs')
    data=upgrade.ops.upstream.account(view['programdata'])
    require(data is not None and data['owner']==LOADER and not data['executable'] and len(data['raw'])>45
            and data['raw'][:4]==(3).to_bytes(4,'little') and int.from_bytes(data['raw'][4:12],'little')==view['deployment_slot']
            and hashlib.sha256(data['raw']).hexdigest()==view['sha256'],'retained Coordinator finalized code differs')
    if value['version']==2:
        image=frozen_coordinator(checked['control'],value['coordinator_elf_sha256'])
        require(data['raw'][45:45+len(image)]==image and not any(data['raw'][45+len(image):]),
                'finalized Coordinator does not match the frozen reviewed ELF')
    config=value['coordinator_config'];account=upgrade.ops.upstream.account(config['address'])
    require(account is not None and account['owner']==worker['coordinator'] and not account['executable']
            and len(account['raw'])==360 and hashlib.sha256(account['raw']).hexdigest()==config['sha256'],
            'retained Coordinator configuration differs')
    raw=account['raw'];require(raw[49] in (0,1),'retained Coordinator authority encoding differs')
    offset=253+32*raw[49]
    require(raw[:8]==hashlib.sha256(b'account:ProgramConfigAccount').digest()[:8] and raw[8]==1
            and tuple(int.from_bytes(raw[offset+index:offset+index+2],'little') for index in (0,2,4))==(24_000,24_000,65_535)
            and int.from_bytes(raw[offset+6:offset+10],'little')==config['max_callback_compute'],
            'retained Coordinator live deadline budget differs')
    binary=SIGNER_CURRENT.parent/value['source_revision']/'vrf-request-signer'
    expected={binary:value['signer']['binary_sha256'],SIGNER_CONFIG:value['signer']['configuration_sha256'],SIGNER_UNIT:value['signer']['unit_sha256']}
    expected.update({ADMISSIONS:None if carried else value['signer']['admissions_sha256'],
                     JOURNAL/'public-state.json':None if carried else value['relay_journal_sha256']})
    observed=upgrade.inspect(list(expected))
    for path,digest in expected.items():
        metadata=observed[str(path)]
        require(sha(metadata.get('sha256')) and (digest is None or metadata['sha256']==digest)
                and type(metadata.get('mode')) is int and 0<=metadata['mode']<=0o777
                and not metadata['mode']&0o022,'retained VRF installed file differs')
        if path in (binary,SIGNER_CONFIG,SIGNER_UNIT):require(metadata.get('uid')==0,'retained signer artifact ownership differs')
        if path in (ADMISSIONS,JOURNAL/'public-state.json'):
            require(metadata['mode']==0o600 and type(metadata.get('uid')) is int and metadata['uid']>0,'retained VRF private journal differs')
            require((metadata['uid']!=before['uid']) if path==ADMISSIONS else (metadata['uid']==before['uid']),
                    'retained VRF journal privilege boundary differs')
    target=upgrade.ops.execute(['readlink','-f',SIGNER_CURRENT]).strip()
    require(target==str(binary.parent),'retained signer artifact link differs')
    raw=upgrade.ops.execute(['systemctl','show','--property=MainPID,ExecStart,FragmentPath,DropInPaths',SIGNER])
    properties=dict(line.split('=',1) for line in raw.splitlines() if '=' in line)
    commands=re.findall(r'argv\[\]=([^;]*);',properties.get('ExecStart',''))
    require(properties.get('MainPID')==str(value['signer']['pid']) and properties.get('FragmentPath')==str(SIGNER_UNIT)
            and properties.get('DropInPaths')=='' and len(commands)==1
            and shlex.split(commands[0])==[str(SIGNER_CURRENT/'vrf-request-signer'),'--config',str(SIGNER_CONFIG)],
            'retained signer effective service differs')
    upgrade.program_floor=max(upgrade.program_floor,view['deployment_slot'])
