#!/usr/bin/env python3
"""Sonic: Model a same-ID Portal update without service or network access."""
import copy
import json
from pathlib import Path
import runpy
import unittest

import continuity_devnet as rollout
from continuity_rollout_state import ARTIFACTS,atomic_json,file_hash,private_json
import portal_program_upgrade as patch

MODEL=runpy.run_path(str(Path(__file__).with_name('test-native-upgrade.py')))


class PatchHost(MODEL['UpgradeHost']):
    def execute(self,arguments,timeout=120,allowed=False):
        args=list(map(str,arguments))
        if args[:3]==['git','-C',str(patch.ROOT)] and 'rev-parse' in args:return self.args.revision+'\n'
        if len(args)>1 and args[1]=='address':return self.world.session_identity['authority']+'\n'
        if args[0]=='solana-keygen' and 'portal-upgrade-buffer.json' in args[-1]:
            if 'new' in args:atomic_json(Path(args[-1]),[0]*64);return ''
            return rollout.public_text(bytes([19])*32)+'\n'
        if 'program' in args and 'deploy' in args:
            self.world.patch_submissions+=1
            plan=self.world.base_plan
            program=plan['programs']['portal']
            key=rollout.public_text(self.world.accounts[program]['raw'][4:])
            stored=self.world.accounts[key]
            stored['raw']=stored['raw'][:4]+(123).to_bytes(8,'little')+stored['raw'][12:45]+(self.args.stage/ARTIFACTS['portal']).read_bytes()
            if self.world.patch_response_lost:raise RuntimeError('modeled uncertain upload receipt')
            return json.dumps({'signature':rollout.public_text(bytes([7])*64)})
        return super().execute(arguments,timeout,allowed)


class PortalProgramUpgradeTests(unittest.TestCase):
    def setUp(self):
        fixture=MODEL['UpgradeTests']();fixture.setUp();self.addCleanup(fixture.doCleanups)
        self.world,self.base=fixture.world,fixture.base
        self.world.base_plan=private_json(self.base/'control/plan.json')
        self.world.args.context=self.world.session_identity['context']
        self.world.args.max_estimated_cost_lamports=8_000_000_000
        self.world.patch_submissions=0;self.world.patch_response_lost=False
        portal=self.world.args.stage/ARTIFACTS['portal'];portal.write_bytes(b'\x7fELFportal-guard')
        self.world.accounts[rollout.public_text(bytes([13])*32)]['lamports']=1000
        self.before=copy.deepcopy(self.world.current)
        self.starts=self.world.starts;self.old_uploads=self.world.uploads[:]
        self.host=PatchHost(self.world)
        original=self.host.upstream.call
        def call(method,params=None):
            if method=='getTransaction':return {'slot':123,'meta':{'err':None},'transaction':{'signatures':[rollout.public_text(bytes([7])*64)]}}
            return original(method,params)
        self.host.upstream.call=call
        self.world.args.review_sha256=patch.ProgramUpgrade(self.host).run(inspect_only=True)['review_sha256']

    def test_inspection_and_wrong_review_never_create_keys_or_submit(self):
        operation=patch.ProgramUpgrade(self.host)
        report=operation.run(inspect_only=True)
        self.assertEqual(len(report['review_sha256']),64)
        self.assertFalse(operation.root.exists())
        self.world.args.review_sha256='0'*64
        with self.assertRaisesRegex(ValueError,'review digest differs'):operation.run()
        self.assertFalse(operation.root.exists())
        self.assertEqual(self.world.patch_submissions,0)

    def test_same_id_update_qualifies_receipt_without_native_restart(self):
        value=patch.ProgramUpgrade(self.host).run()
        self.assertEqual(self.world.patch_submissions,1)
        self.assertEqual(self.world.starts,self.starts)
        self.assertEqual(self.world.current,self.before)
        self.assertEqual(self.world.uploads,self.old_uploads)
        self.assertEqual(value['before_programs']['portal']['programdata'],value['after_programs']['portal']['programdata'])
        self.assertEqual(value['before_programs']['token_bridge'],value['after_programs']['token_bridge'])
        self.assertEqual(value['identity'],self.world.session_identity)
        self.assertEqual(patch.read_receipt(self.world.args.state_root,value['receipt_sha256'])['result'],'passed')

    def test_uncertain_post_submission_result_preserves_state_and_forbids_resubmission(self):
        self.world.patch_response_lost=True
        operation=patch.ProgramUpgrade(self.host)
        with self.assertRaisesRegex(RuntimeError,'uncertain upload'):operation.run()
        self.assertTrue((operation.control/'submit-attempted.json').exists())
        self.assertFalse((operation.control/'qualified.json').exists())
        with self.assertRaisesRegex(ValueError,'explicit reconciliation'):operation.run()
        self.assertEqual(self.world.patch_submissions,1)
        self.assertEqual(self.world.starts,self.starts)
        self.assertEqual(self.world.current,self.before)

    def test_preflight_binding_failure_does_not_create_a_submission_namespace(self):
        self.world.snapshot_error='snapshot unavailable'
        operation=patch.ProgramUpgrade(self.host)
        with self.assertRaisesRegex(ValueError,'snapshot unavailable'):operation.run()
        self.assertFalse(operation.root.exists())
        self.assertEqual(self.world.patch_submissions,0)
        self.assertEqual(self.world.starts,self.starts)

    def test_original_program_and_context_qualification_are_required_before_submission(self):
        path=self.base/'control/programs.json';uploads=private_json(path)
        changed=copy.deepcopy(uploads);changed['roles']['portal']['elf_sha256']='0'*64
        atomic_json(path,changed)
        operation=patch.ProgramUpgrade(self.host)
        with self.assertRaisesRegex(ValueError,'upload observations'):operation.run(inspect_only=True)
        atomic_json(path,uploads)
        report_path=self.base/'control/report.json';report=private_json(report_path);report['result']='failed'
        atomic_json(report_path,report)
        with self.assertRaisesRegex(ValueError,'context is not qualified'):operation.run(inspect_only=True)
        self.assertFalse(operation.root.exists())
        self.assertEqual(self.world.patch_submissions,0)

    def test_existing_challenged_checkpoint_is_not_a_program_update_recovery(self):
        identity=self.world.session_identity
        cursor=bytearray(153);cursor[:8]=bytes([251,223,216,188,51,244,129,196]);cursor[8:40]=rollout.public_bytes(identity['session'])
        cursor[112:144]=bytes([9])*32;cursor[144:152]=(7).to_bytes(8,'little')
        checkpoint=bytearray(380);checkpoint[:8]=bytes([199,62,186,186,98,119,211,139]);checkpoint[8:40]=cursor[8:40]
        checkpoint[40:48]=cursor[144:152]
        account={'owner':identity['portal'],'raw':bytes(cursor)}
        patch.challenge_guard(account,{'owner':identity['portal'],'raw':bytes(checkpoint)},identity['portal'],identity['session'])
        checkpoint[328]=4
        with self.assertRaisesRegex(ValueError,'separate reconciliation'):
            patch.challenge_guard(account,{'owner':identity['portal'],'raw':bytes(checkpoint)},identity['portal'],identity['session'])

    def test_peak_quote_rejects_out_of_scope_funding(self):
        self.assertEqual(patch.quote(100,200,50,8_000_000_000),200_000_250)
        for arguments in ((8_000_000_000,0,0,8_000_000_000),(1,1,0,9_000_000_000),(-1,1,0,8_000_000_000)):
            with self.assertRaises(ValueError):patch.quote(*arguments)


if __name__=='__main__':unittest.main()
