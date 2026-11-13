"""Sonic: Read-only admission of a qualified retained ER-VRF handoff."""
import hashlib
import json
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


def checksum(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def sha(value):
    return isinstance(value,str) and re.fullmatch('[0-9a-f]{64}',value) is not None


def revision(value):
    return isinstance(value,str) and re.fullmatch('[0-9a-f]{40}',value) is not None


def validate_receipt(value):
    require(isinstance(value,dict) and set(value)==FIELDS and type(value['version']) is int and value['version']==1
            and value['kind']=='retained-er-vrf-handoff' and value['result']=='passed','retained VRF handoff schema differs')
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
            and all(sha(digest) for digest in retained.values()) and retained['coordinator_programdata_sha256']==before['sha256'],
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


def admit(upgrade,profile,previous,before,carried):
    from native_upgrade import service_spec
    from vrf_relay_profile import JOURNAL, configuration_hash
    value=read_receipt(profile['retained_upgrade_receipt_sha256'])
    worker=profile['configuration']['worker'];signer=profile['configuration']['signer']
    require(upgrade.recovery_slot is None and value['identity']['context']==profile['context']
            and value['identity']['session']==worker['er_session']
            and value['base_revision']==upgrade.base.name and value['base_plan_sha256']==checksum(upgrade.base_plan)
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
