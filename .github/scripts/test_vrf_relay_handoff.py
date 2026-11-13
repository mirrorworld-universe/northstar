import copy
import hashlib
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_vrf_relay_profile as profiles
import vrf_relay_handoff as handoff
import vrf_relay_profile as relay
from continuity_devnet import LOADER, public_bytes, public_text
from continuity_rollout_state import DEVNET
from native_upgrade import STATE_KEYS


class HandoffTests(unittest.TestCase):
    def fixture(self):
        previous,identity=profiles.RelayProfileTests().fixture()
        profile=copy.deepcopy(previous)
        data_address=public_text(bytes([12])*32)
        raw=(3).to_bytes(4,'little')+(50).to_bytes(8,'little')+b'\x01'+bytes([11])*32+b'\x7fELFretained'
        after={'programdata':data_address,'deployment_slot':50,'sha256':hashlib.sha256(raw).hexdigest()}
        retained={'policy_sha256':'44'*32,'coordinator_programdata_sha256':'22'*32}
        profile['configuration']['worker']['coordinator_programdata_sha256']=after['sha256']
        profile['configuration']['signer'].update(policy_sha256='66'*32,retained_policy=retained)
        profile['retained_upgrade_receipt_sha256']='77'*32
        identity.update(version=2,genesis=DEVNET,portal=public_text(bytes([7])*32),token_bridge=public_text(bytes([8])*32),
                        validator=public_text(bytes([9])*32),profile='99'*32)
        plan={'programs':{name:identity[name] for name in ('portal','token_bridge')},'validator':identity['validator'],'profile':identity['profile']}
        before={'arguments':['/old/runtime/bin/agave-validator'],'binary_sha256':'aa'*32,'uid':1000,'gid':100,
                'state_environment':{name:'/retained/'+str(index) for index,name in enumerate(STATE_KEYS)}}
        config=bytearray(360);config[:8]=hashlib.sha256(b'account:ProgramConfigAccount').digest()[:8];config[8]=1
        config[253:263]=(24_000).to_bytes(2,'little')*2+(65_535).to_bytes(2,'little')+(400_000).to_bytes(4,'little')
        config_address=public_text(bytes([13])*32)
        source=json.loads((Path(__file__).resolve().parents[2]/'northstar/vrf-relay/source-lock.json').read_text())['revision']
        value={'version':1,'kind':'retained-er-vrf-handoff','result':'passed','source_revision':'3'*40,
            'relay_source_revision':source,'target_native_revision':'2'*40,'base_revision':'1'*40,
            'base_plan_sha256':handoff.checksum(plan),'native_before':before,'identity':identity,'er_genesis':'11111111111111111111111111111111',
            'old_configuration_sha256':relay.configuration_hash(previous),'new_configuration_sha256':relay.configuration_hash(profile),
            'coordinator_before':{'programdata':data_address,'deployment_slot':40,'sha256':'22'*32},'coordinator_after':after,
            'coordinator_elf_sha256':'bb'*32,'coordinator_config':{'address':config_address,'sha256':hashlib.sha256(config).hexdigest(),
                'default_deadline_slots':24_000,'min_deadline_slots':24_000,'max_deadline_slots':65_535,'max_callback_compute':400_000},
            'signer':{'binary_sha256':'cc'*32,'configuration_sha256':'dd'*32,'unit_sha256':'ee'*32,'admissions_sha256':'88'*32,'pid':9},
            'relay_journal_sha256':'ff'*32,'retained_policy':retained,'peak_lamports':5_000_000_000,
            'native_service_actions':0,'funding_transactions':0,'program_submission_passes':1,'signer_starts':1}
        accounts={profile['configuration']['worker']['coordinator']:{'owner':LOADER,'executable':True,'raw':(2).to_bytes(4,'little')+public_bytes(data_address)},
                  data_address:{'owner':LOADER,'executable':False,'raw':raw},
                  config_address:{'owner':profile['configuration']['worker']['coordinator'],'executable':False,'raw':bytes(config)}}
        binary=handoff.SIGNER_CURRENT.parent/value['source_revision']/'vrf-request-signer'
        files={str(binary):{'uid':0,'mode':0o555,'sha256':value['signer']['binary_sha256']},
            str(handoff.SIGNER_CONFIG):{'uid':0,'mode':0o644,'sha256':value['signer']['configuration_sha256']},
            str(handoff.SIGNER_UNIT):{'uid':0,'mode':0o644,'sha256':value['signer']['unit_sha256']},
            str(handoff.ADMISSIONS):{'uid':65000,'mode':0o600,'sha256':value['signer']['admissions_sha256']},
            str(relay.JOURNAL/'public-state.json'):{'uid':1000,'mode':0o600,'sha256':value['relay_journal_sha256']}}
        commands=[]
        def execute(command):
            commands.append(list(map(str,command)))
            if command[0]=='readlink':return str(binary.parent)+'\n'
            if command[0]=='systemctl':return ('MainPID=9\nFragmentPath='+str(handoff.SIGNER_UNIT)+'\nDropInPaths=\nExecStart={ argv[]='
                +str(handoff.SIGNER_CURRENT/'vrf-request-signer')+' --config '+str(handoff.SIGNER_CONFIG)+' ; }\n')
            return ''
        upgrade=SimpleNamespace(base=Path('/contexts')/value['base_revision'],base_plan=plan,recovery_slot=None,program_floor=10,
            args=SimpleNamespace(revision='4'*40),ops=SimpleNamespace(upstream=SimpleNamespace(account=accounts.get),execute=execute),
            inspect=lambda paths:{str(path):files[str(path)] for path in paths})
        return value,profile,previous,before,upgrade,files,accounts,commands

    def test_receipt_requires_typed_scope_counts_and_retained_native_binding(self):
        value,*_=self.fixture();handoff.validate_receipt(value)
        for name,replacement in (('native_service_actions',1),('funding_transactions',1),('program_submission_passes',2),
                                 ('signer_starts',True),('peak_lamports',handoff.LIMIT+1),('native_before',None)):
            changed=copy.deepcopy(value);changed[name]=replacement
            with self.subTest(name=name),self.assertRaises(ValueError):handoff.validate_receipt(changed)
        for name in ('programdata','deployment_slot','sha256'):
            changed=copy.deepcopy(value);changed['coordinator_after'][name]=changed['coordinator_before'][name]
            if name=='programdata':changed['coordinator_after'][name]=public_text(bytes([14])*32)
            with self.subTest(name=name),self.assertRaises(ValueError):handoff.validate_receipt(changed)
        changed=copy.deepcopy(value);changed['coordinator_config']['max_deadline_slots']=1000
        with self.assertRaises(ValueError):handoff.validate_receipt(changed)

    def test_new_handoff_checks_finalized_code_frozen_journals_and_signer_without_mutation(self):
        value,profile,previous,before,upgrade,files,accounts,commands=self.fixture()
        with patch.object(handoff,'read_receipt',return_value=value):handoff.admit(upgrade,profile,previous,before,False)
        self.assertEqual(upgrade.program_floor,50)
        self.assertTrue(all(command[0] in ('git','readlink','systemctl') for command in commands))
        files[str(handoff.ADMISSIONS)]['sha256']='00'*32
        with patch.object(handoff,'read_receipt',return_value=value),self.assertRaises(ValueError):
            handoff.admit(upgrade,profile,previous,before,False)
        files[str(handoff.ADMISSIONS)]['sha256']=value['signer']['admissions_sha256']
        accounts[value['coordinator_after']['programdata']]['raw']+=b'changed'
        with patch.object(handoff,'read_receipt',return_value=value),self.assertRaises(ValueError):
            handoff.admit(upgrade,profile,previous,before,False)

    def test_carried_receipt_allows_new_admissions_but_not_changed_signer_artifacts(self):
        value,profile,previous,before,upgrade,files,*_=self.fixture()
        files[str(handoff.ADMISSIONS)]['sha256']='00'*32
        files[str(relay.JOURNAL/'public-state.json')]['sha256']='00'*32
        with patch.object(handoff,'read_receipt',return_value=value):handoff.admit(upgrade,profile,profile,before,True)
        files[str(handoff.SIGNER_CONFIG)]['sha256']='00'*32
        with patch.object(handoff,'read_receipt',return_value=value),self.assertRaises(ValueError):
            handoff.admit(upgrade,profile,profile,before,True)


if __name__=='__main__':unittest.main()
