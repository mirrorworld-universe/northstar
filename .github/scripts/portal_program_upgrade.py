#!/usr/bin/env python3
"""Sonic: Same-ID devnet Portal upgrade with retained-state qualification."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import stat
import tempfile
import time

from continuity_rollout_state import (
    ARTIFACTS, DEVNET, atomic_json, canonical, checked_directory, file_hash,
    immutable_json, private_json, require,
)

ROOT = Path(__file__).resolve().parents[2]
FIX_COMMIT = '3171d9e549273fb2fc61a74dae66667c23619999'
LIMIT = 8_000_000_000
RECEIPT_FIELDS = {
    'version','kind','result','source_revision','reviewed_fix_commit','base_revision','base_plan_sha256',
    'native_before','identity','er_genesis','before_programs','after_programs','before_portal_elf_sha256',
    'portal_elf_sha256','token_bridge_elf_sha256','transaction_signature','peak_lamports','net_cost_lamports','native_service_actions',
}


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def receipt_path(root, checksum):
    require(isinstance(checksum,str) and re.fullmatch('[0-9a-f]{64}',checksum),'exact Portal receipt digest required')
    return root/'portal-program-upgrades/receipts'/(checksum+'.json')


def read_receipt(root, checksum):
    path=receipt_path(root,checksum)
    checked_directory(path.parent,private=True)
    value=private_json(path)
    require(digest(value)==checksum,'Portal receipt digest differs')
    validate_receipt(value)
    return value


def validate_receipt(value):
    require(isinstance(value,dict) and set(value)==RECEIPT_FIELDS and value['version']==1
            and value['kind']=='portal-program-upgrade' and value['result']=='passed'
            and value['reviewed_fix_commit']==FIX_COMMIT and value['native_service_actions']==0,
            'Portal upgrade receipt schema or qualification differs')
    for field in ('source_revision','base_revision'):
        require(isinstance(value[field],str) and re.fullmatch('[0-9a-f]{40}',value[field]),'Portal source binding differs')
    for field in ('base_plan_sha256','before_portal_elf_sha256','portal_elf_sha256','token_bridge_elf_sha256'):
        require(isinstance(value[field],str) and re.fullmatch('[0-9a-f]{64}',value[field]),'Portal artifact binding differs')
    require(type(value['peak_lamports']) is int and 0<value['peak_lamports']<=LIMIT
            and type(value['net_cost_lamports']) is int and 0<=value['net_cost_lamports']<=value['peak_lamports']
            and isinstance(value['transaction_signature'],str)
            and re.fullmatch('[1-9A-HJ-NP-Za-km-z]{80,90}',value['transaction_signature']),
            'Portal receipt cost or transaction binding differs')
    require(value['before_portal_elf_sha256']!=value['portal_elf_sha256'],'Portal receipt does not change reviewed bytes')
    before,after=value['before_programs'],value['after_programs']
    require(isinstance(before,dict) and isinstance(after,dict) and set(before)==set(after)=={'portal','token_bridge'}
            and before['token_bridge']==after['token_bridge'],'Portal upgrade changed bridge state')
    for view in (*before.values(),*after.values()):
        require(isinstance(view,dict) and set(view)=={'programdata','deployment_slot','sha256'}
                and isinstance(view['programdata'],str) and re.fullmatch('[1-9A-HJ-NP-Za-km-z]{32,44}',view['programdata'])
                and type(view['deployment_slot']) is int and 0<=view['deployment_slot']<1<<64
                and isinstance(view['sha256'],str) and re.fullmatch('[0-9a-f]{64}',view['sha256']),
                'Portal receipt program view differs')
    require(before['portal']['programdata']==after['portal']['programdata']
            and after['portal']['deployment_slot']>before['portal']['deployment_slot']
            and after['portal']['sha256']!=before['portal']['sha256'],'Portal identity or deployment progression differs')
    identity=value['identity']
    require(isinstance(identity,dict) and identity.get('version')==2 and identity.get('genesis')==DEVNET
            and isinstance(identity.get('context'),str) and re.fullmatch('[0-9a-f]{64}',identity['context'])
            and isinstance(identity.get('session'),str) and value['er_genesis']=='11111111111111111111111111111111',
            'Portal retained session binding differs')


def admit(upgrade,receipt,before,carried):
    validate_receipt(receipt)
    require(getattr(upgrade,'recovery_slot',None) is None,'Portal receipt is not a recovery capability')
    require(receipt['base_revision']==upgrade.base.name and receipt['base_plan_sha256']==digest(upgrade.base_plan)
            and receipt['before_portal_elf_sha256']==upgrade.base_manifest['sha256']['portal']
            and receipt['portal_elf_sha256']==upgrade.manifest['sha256']['portal']
            and receipt['token_bridge_elf_sha256']==upgrade.manifest['sha256']['token_bridge']==upgrade.base_manifest['sha256']['token_bridge'],
            'Portal receipt does not bind these retained artifacts')
    identity=receipt['identity']; plan=upgrade.base_plan
    require(all(identity.get(name)==plan['programs'][name] for name in ('portal','token_bridge'))
            and identity.get('validator')==plan['validator'] and identity.get('profile')==plan['profile'],
            'Portal receipt does not retain the qualified context')
    if not carried:
        from native_upgrade import service_spec
        require(receipt['native_before']==service_spec(before),'Portal receipt original Native binding differs')
    upgrade.ops.execute(['git','merge-base','--is-ancestor',FIX_COMMIT,upgrade.args.revision])
    upgrade.ops.execute(['git','merge-base','--is-ancestor',receipt['source_revision'],upgrade.args.revision])


def challenge_guard(cursor,checkpoint,portal,session):
    if cursor is None:return
    from continuity_devnet import public_bytes
    raw=cursor['raw']
    require(cursor['owner']==portal and len(raw)==153 and raw[:8]==bytes([251,223,216,188,51,244,129,196])
            and raw[8:40]==public_bytes(session),'Portal cursor binding differs')
    if not any(raw[112:144]):return
    require(checkpoint is not None and checkpoint['owner']==portal,'active checkpoint is unavailable')
    value=checkpoint['raw']
    require(len(value)==380 and value[:8]==bytes([199,62,186,186,98,119,211,139]) and value[8:40]==raw[8:40]
            and value[40:48]==raw[144:152] and value[328] in (0,1,3),
            'existing challenged or invalid checkpoint requires separate reconciliation')


def quote(buffer_rent,program_rent,current_lamports,cap):
    require(type(cap) is int and 0<cap<=LIMIT,'Portal peak cap exceeds approved devnet funding')
    require(all(type(value) is int and value>=0 for value in (buffer_rent,program_rent,current_lamports)),
            'Portal rent quote is malformed')
    peak=buffer_rent+max(0,program_rent-current_lamports)+200_000_000
    require(peak<=cap,'Portal buffer and extension quote exceeds approved peak')
    return peak


class ProgramUpgrade:
    def __init__(self,operations):
        self.ops,self.args=operations,operations.args
        self.root=self.args.state_root/'portal-program-upgrades'/self.args.revision
        self.control=self.root/'control'

    def unchanged(self):
        require(self.ops.inventory()==self.before,'Native binding changed during Portal upgrade')

    def ensure_unblocked(self):
        from continuity_devnet import public_text
        derive="const {createRequire}=require('module');const path=require('path');const r=createRequire(path.join(process.argv[1],'package.json'));const {PublicKey}=r('@solana/web3.js');console.log(PublicKey.findProgramAddressSync([Buffer.from('checkpoint_cursor'),new PublicKey(process.argv[3]).toBuffer()],new PublicKey(process.argv[2]))[0].toBase58())"
        identity=self.snapshot['identity']
        address=self.ops.execute(['node','-e',derive,self.args.sdk_checkout,identity['portal'],identity['session']]).strip()
        cursor=self.ops.upstream.account(address)
        active=cursor['raw'][112:144] if cursor is not None else bytes(32)
        checkpoint=self.ops.upstream.account(public_text(active)) if len(active)==32 and any(active) else None
        challenge_guard(cursor,checkpoint,identity['portal'],identity['session'])

    def run(self,inspect_only=False):
        from continuity_devnet import DROPIN,SERVICE
        from native_upgrade import NativeUpgrade,service_spec
        require(not os.path.lexists(self.root),'existing Portal upgrade requires explicit reconciliation')
        require(self.ops.upstream.call('getGenesisHash')==self.ops.l1.call('getGenesisHash')==DEVNET,'Portal upgrade is devnet-only')
        self.ops.verify_sdk()
        require(self.ops.execute(['git','-C',ROOT,'rev-parse','HEAD']).strip()==self.args.revision,'Portal source checkout differs')
        self.ops.execute(['git','-C',ROOT,'diff','--quiet','HEAD'])
        self.ops.execute(['git','-C',ROOT,'merge-base','--is-ancestor',FIX_COMMIT,self.args.revision])
        self.before=self.ops.inventory()
        upgrade=NativeUpgrade(self.ops,SERVICE,DROPIN)
        with tempfile.TemporaryDirectory(prefix='northstar-portal-preflight-') as temporary:
            self.ops.control=Path(temporary)
            upgrade.control=self.ops.control
            upgrade.load_retained_base(self.before)
            upgrade.binding(self.before)
            require(getattr(upgrade,'previous_portal_receipt',None) is None,'chained Portal program changes require separate review')
            self.base_plan=upgrade.base_plan
            self.before_programs={role:self.ops.program_view(role,stage=upgrade.base_stage,plan=self.base_plan) for role in ('portal','token_bridge')}
            uploads=private_json(upgrade.base_control/'programs.json')
            require(all(uploads['roles'][role]['complete']==self.before_programs[role] for role in self.before_programs),
                    'retained program upload observations differ')
            self.hashes={role:file_hash(self.args.stage/ARTIFACTS[role]) for role in ('portal','token_bridge')}
            require(self.hashes['token_bridge']==upgrade.base_manifest['sha256']['token_bridge']
                    and self.hashes['portal']!=upgrade.base_manifest['sha256']['portal'],'only reviewed Portal bytes may change')
            self.snapshot=upgrade.snapshot('online')
            require(self.snapshot['identity']['context']==self.args.context,'reviewed Portal context differs')
            self.ensure_unblocked()
            self.cli=upgrade.base_stage/ARTIFACTS['cli']
            funder=self.ops.execute([self.cli,'address','--keypair',self.args.funder_keypair]).strip()
            require(funder==self.base_plan['funder'],'Portal upgrade requires the retained CI authority')
            account=self.ops.upstream.account(self.before_programs['portal']['programdata'])
            length=(self.args.stage/ARTIFACTS['portal']).stat().st_size
            peak=quote(self.ops.l1.call('getMinimumBalanceForRentExemption',[37+length]),
                       self.ops.l1.call('getMinimumBalanceForRentExemption',[45+length]),account['lamports'],self.args.max_estimated_cost_lamports)
            funding_balance=self.ops.upstream.call('getBalance',[funder,{'commitment':'finalized'}])['value']
            require(type(funding_balance) is int and funding_balance>=peak,'CI authority balance is below Portal peak quote')
        reviewed={'version':1,'source_revision':self.args.revision,'base_revision':upgrade.base.name,
            'base_plan_sha256':digest(self.base_plan),'native_before':service_spec(self.before),
            'native_pid':self.before['pid'],'native_start_time':self.before['start_time'],
            'before_programs':self.before_programs,'artifacts':self.hashes,'snapshot':self.snapshot,
            'funder':funder,'funding_balance':funding_balance,'peak_lamports':peak}
        review_sha256=digest(reviewed)
        if inspect_only:
            report={'version':1,'scope':'read-only Portal preflight; no key creation, funding, upload or service action',
                'review':reviewed,'review_sha256':review_sha256}
            print(json.dumps(report),flush=True)
            return report
        require(getattr(self.args,'review_sha256',None)==review_sha256,'Portal preflight review digest differs')
        parent=self.root.parent
        if not os.path.lexists(parent):parent.mkdir(mode=0o711)
        checked_directory(parent)
        self.root.mkdir(mode=0o711);self.control.mkdir(mode=0o700);(self.control/'keys').mkdir(mode=0o700)
        self.ops.control=self.control;self.ops.log=self.control/'operations.log'
        buffer=self.ops.key('portal-upgrade-buffer',False)
        self.plan={'version':1,'source_revision':self.args.revision,'base_revision':upgrade.base.name,
            'base_plan_sha256':digest(self.base_plan),'native_before':service_spec(self.before),'before_programs':self.before_programs,
            'artifacts':self.hashes,'snapshot':self.snapshot,'buffer':buffer,'peak_lamports':peak,'review_sha256':review_sha256}
        immutable_json(self.control/'plan.json',self.plan)
        self.unchanged()
        require(self.hashes=={role:file_hash(self.args.stage/ARTIFACTS[role]) for role in self.hashes},'reviewed program bytes changed before submission')
        require(self.ops.upstream.account(buffer) is None,'fresh Portal buffer address already exists')
        immutable_json(self.control/'submit-attempted.json',{'version':1,'plan_sha256':digest(self.plan)})
        upgrade.control=self.control
        raw=self.ops.execute([self.cli,'--url',self.args.l1_rpc,'--commitment','finalized','--keypair',self.args.funder_keypair,
            '--output','json','program','deploy',self.args.stage/ARTIFACTS['portal'],'--program-id',self.base_plan['programs']['portal'],
            '--upgrade-authority',self.args.funder_keypair,'--buffer',self.control/'keys/portal-upgrade-buffer.json',
            '--max-len',str((self.args.stage/ARTIFACTS['portal']).stat().st_size),'--use-rpc','--max-sign-attempts','1','--max-deploy-attempts','1'],timeout=900)
        result=json.loads(raw)
        signature=result.get('signature')
        require(isinstance(signature,str),'Portal uploader did not return a transaction signature')
        deadline=time.monotonic()+180
        while True:
            transaction=self.ops.upstream.call('getTransaction',[signature,{'commitment':'finalized','maxSupportedTransactionVersion':0}])
            if transaction is not None:break
            require(time.monotonic()<deadline,'Portal upload receipt is not finalized')
            time.sleep(0.5)
        require(transaction.get('meta',{}).get('err') is None and transaction['transaction']['signatures'][0]==signature,
                'Portal upload transaction failed or differs')
        after={role:self.ops.program_view(role,stage=self.args.stage,plan=self.base_plan) for role in ('portal','token_bridge')}
        require(type(transaction.get('slot')) is int and transaction['slot']==after['portal']['deployment_slot'],
                'Portal upgrade transaction and deployment slot differ')
        require(self.ops.upstream.account(buffer) is None,'Portal upload buffer remains; reconcile without another submission')
        self.unchanged()
        upgraded=upgrade.snapshot('online')
        require(upgraded==self.snapshot,'Portal upgrade changed the retained session or ER genesis')
        self.ensure_unblocked()
        remaining=self.ops.upstream.call('getBalance',[funder,{'commitment':'finalized','minContextSlot':after['portal']['deployment_slot']}])['value']
        require(type(remaining) is int and 0<=funding_balance-remaining<=peak,'Portal net funding observation differs')
        receipt={'version':1,'kind':'portal-program-upgrade','result':'passed','source_revision':self.args.revision,
            'reviewed_fix_commit':FIX_COMMIT,'base_revision':upgrade.base.name,'base_plan_sha256':digest(self.base_plan),
            'native_before':service_spec(self.before),'identity':self.snapshot['identity'],'er_genesis':self.snapshot['er_genesis'],
            'before_programs':self.before_programs,'after_programs':after,
            'before_portal_elf_sha256':upgrade.base_manifest['sha256']['portal'],'portal_elf_sha256':self.hashes['portal'],
            'token_bridge_elf_sha256':self.hashes['token_bridge'],'transaction_signature':signature,'peak_lamports':peak,
            'net_cost_lamports':funding_balance-remaining,'native_service_actions':0}
        validate_receipt(receipt)
        registry=receipt_path(self.args.state_root,digest(receipt))
        if not os.path.lexists(registry.parent):registry.parent.mkdir(mode=0o700)
        checked_directory(registry.parent,private=True)
        immutable_json(registry,receipt)
        immutable_json(self.control/'qualified.json',receipt)
        report={**receipt,'receipt_sha256':digest(receipt),'scope':'same-ID Portal program update; no Native restart or session/journal reset'}
        immutable_json(self.control/'report.json',report)
        print(json.dumps(report),flush=True)
        if os.environ.get('GITHUB_OUTPUT'):
            with open(os.environ['GITHUB_OUTPUT'],'a') as output:output.write('report_path='+str(self.control/'report.json')+'\n')
        return report


def main():
    from continuity_devnet import Deployment
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--revision',required=True)
    parser.add_argument('--stage',type=Path,required=True)
    parser.add_argument('--sdk-checkout',type=Path,required=True)
    parser.add_argument('--funder-keypair',type=Path,required=True)
    parser.add_argument('--context',required=True)
    parser.add_argument('--inspect-only',action='store_true')
    parser.add_argument('--review-sha256')
    parser.add_argument('--state-root',type=Path,default=Path('/solana/northstar/continuity-v2'))
    parser.add_argument('--l1-rpc',default='http://127.0.0.1:8899')
    parser.add_argument('--er-rpc',default='http://127.0.0.1:8910')
    parser.add_argument('--devnet-rpc',default='https://api.devnet.solana.com')
    parser.add_argument('--max-estimated-cost-lamports',type=int,default=LIMIT)
    args=parser.parse_args()
    require(re.fullmatch('[0-9a-f]{40}',args.revision) and re.fullmatch('[0-9a-f]{64}',args.context)
            and all(path.is_absolute() for path in (args.stage,args.sdk_checkout,args.state_root,args.funder_keypair))
            and str(args.funder_keypair)!='/home/ubuntu/.config/solana/id.json','explicit Portal source/context/CI credential required')
    require(args.l1_rpc=='http://127.0.0.1:8899' and args.er_rpc=='http://127.0.0.1:8910'
            and args.devnet_rpc=='https://api.devnet.solana.com','Portal update requires the scoped devnet RPCs')
    require(0<args.max_estimated_cost_lamports<=LIMIT,'Portal funding cap exceeds approval')
    require(args.inspect_only and args.review_sha256 is None or not args.inspect_only
            and re.fullmatch('[0-9a-f]{64}',args.review_sha256 or ''),'Portal submission requires an exact preflight digest')
    key=private_json(args.funder_keypair)
    require(isinstance(key,list) and len(key)==64 and all(type(byte) is int and 0<=byte<=255 for byte in key),'CI authority credential format differs')
    def interrupted(_number,_frame):raise KeyboardInterrupt('Portal submission interrupted; retained state requires reconciliation')
    signal.signal(signal.SIGTERM,interrupted)
    descriptor=os.open('/solana/northstar/continuity-v2-deploy.lock',os.O_RDWR|os.O_NOFOLLOW|os.O_NONBLOCK)
    try:
        info=os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_uid==os.getuid() and not info.st_mode&0o077,'Portal deployment lock differs')
        fcntl.flock(descriptor,fcntl.LOCK_EX|fcntl.LOCK_NB)
        args.lock_descriptor=descriptor
        ProgramUpgrade(Deployment(args)).run(inspect_only=args.inspect_only)
    finally:os.close(descriptor)


if __name__=='__main__':main()
