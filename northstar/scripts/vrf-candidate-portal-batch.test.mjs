import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { PROGRAM, uint, hex, sha256, candidateDomain, qualifiedSessionIdentity } from './vrf-candidate-portal-codec.mjs';

const script = fileURLToPath(new URL('./vrf-candidate-portal-batch.mjs', import.meta.url));
function specimen() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'vrf-portal-batch-codec-'));
  const field = n => hex(uint(n, 32, 'BE'));
  const digest = text => hex(sha256(Buffer.from(text)));
  const cases = { schema: 'northstar.vrf-candidate-portal-cases.v1', portal_sbf_sha256: digest('synthetic-SBF-not-executable'),
    policy_sha256: digest('synthetic-policy'), program_vkey_hash: field(7), cases: [] };
  const writes = [];
  for (let i = 0; i < 3; i++) {
    const name = `case${i}`;
    const dir = path.join(root, name);
    fs.mkdirSync(path.join(dir, 'public'), { recursive: true });
    const preimage = Buffer.concat([Buffer.from('northstar-session-context-v1'), Buffer.alloc(120), Buffer.from([1])]);
    preimage[100] = i;
    const session = { program_id: PROGRAM, session: 'same-singleton-codec-PDA', validator: `codec-validator${i}`, grid_id: String(i), nonce: '0',
      genesis_hash: '11111111111111111111111111111111', session_context_preimage_hex: hex(preimage) };
    const sessionRaw = JSON.stringify(session);
    fs.writeFileSync(path.join(dir, 'public/session.json'), sessionRaw);
    const publicBytes = Buffer.concat([candidateDomain(), uint(i + 1, 32, 'BE'), Buffer.concat([Buffer.alloc(16), uint(37 + i, 8, 'BE'), uint(0, 8, 'BE')]), ...[10, 11, 12, 13, 14].map(n => uint(n, 32, 'BE'))]);
    const binding = { schema: 'northstar.vrf-candidate-portal-binding.v1', ...session, proof_kind: 3, proof_version: 1, er_slot: String(37 + i), step_index: 0,
      profile_sha256: cases.policy_sha256, state_roots: [field(10), field(11)], transaction_tree_root: field(12), readonly_context_root: field(13), settlement_effect_root: field(14),
      public_inputs_hex: hex(publicBytes), public_inputs_file: 'public.bin', witness_file: 'witness.borsh',
      captures: [{ role: i ? 'fulfillment_callback' : 'request', capture_sha256: digest(`synthetic-capture${i}`), transaction_sha256: digest(`synthetic-wire${i}`) }] };
    const bindingRaw = JSON.stringify(binding);
    fs.writeFileSync(path.join(dir, 'binding.json'), bindingRaw);
    fs.writeFileSync(path.join(dir, 'public.bin'), publicBytes);
    fs.writeFileSync(path.join(dir, 'witness.borsh'), `synthetic-codec-not-an-envelope${i}`);
    const prepared = { schema: 'northstar.vrf-candidate-portal-prepared.v1', kind: 3, version: 1, binding_sha256: digest(bindingRaw), public_inputs_sha256: hex(sha256(publicBytes)),
      session, session_source_sha256: digest(sessionRaw), session_identity_sha256: qualifiedSessionIdentity(session), er_slot: binding.er_slot, step_index: 0,
      portal_program: { elf_sha256: cases.portal_sbf_sha256 }, deadline_slot: '1000', prepared_slot: 10, addresses: { checkpoint: `codec-checkpoint${i}`, challenge: `codec-challenge${i}` } };
    fs.writeFileSync(path.join(dir, 'public/prepared.json'), JSON.stringify(prepared));
    fs.writeFileSync(path.join(dir, 'public/durable-codec.json'), JSON.stringify({ schema: 'northstar.vrf-candidate-portal-durable.v1', phase: 'prepare', phase_exit: 0, confirmed_fence: 10, snapshot_slot: 20 }));
    writes.push({ dir, session, binding, prepared });
    cases.cases.push({ case_id: name, status: 'prepared', work: name, binding_json: `${name}/binding.json` });
  }
  const validator = path.join(root, 'synthetic-validator-TEST-ONLY.mjs');
  fs.writeFileSync(validator, `#!${process.execPath}
// Synthetic packet-codec test double, never used by a live lifecycle.
import fs from 'node:fs'; import {createHash} from 'node:crypto';
const raw=fs.readFileSync(process.argv[2]); const b=JSON.parse(raw); const h=x=>createHash('sha256').update(x).digest('hex');
console.log(JSON.stringify({schema:'northstar.vrf-candidate-portal-binding-validation.v1',binding_sha256:h(raw),public_inputs_sha256:h(Buffer.from(b.public_inputs_hex,'hex')),capture_sha256:b.captures.map(c=>c.capture_sha256),execution_verified:true}));\n`, { mode: 0o700 });
  const casesPath = path.join(root, 'cases.json');
  fs.writeFileSync(casesPath, JSON.stringify(cases));
  const run = () => spawnSync(process.execPath, [script, casesPath, path.join(root, 'output')], { encoding: 'utf8',
    env: { ...process.env, NORTHSTAR_VRF_BINDING_VALIDATOR: validator, NORTHSTAR_VRF_BINDING_VALIDATOR_SHA256: hex(sha256(fs.readFileSync(validator))) } });
  return { root, cases, casesPath, writes, run };
}

for (const [name, mutate, exit] of [
  ['portable synthetic batch never claims proof or timing success', () => {}, 0],
  ['unprepared case rejects', s => { s.cases.cases[1].status = 'session-ready'; }, 1],
  ['different configured SBF rejects', s => { s.writes[1].prepared.portal_program.elf_sha256 = 'ff'.repeat(32); }, 1],
  ['changed actual-session source rejects', s => { s.writes[1].prepared.session_source_sha256 = 'ff'.repeat(32); }, 1],
  ['changed policy rejects', s => { s.cases.policy_sha256 = 'ff'.repeat(32); }, 1],
  ['duplicate context rejects', s => { s.cases.cases[1].work = 'case0'; s.cases.cases[1].binding_json = 'case0/binding.json'; }, 1],
  ['wrong public artifact rejects', s => { fs.writeFileSync(path.join(s.writes[1].dir, 'public.bin'), Buffer.alloc(256)); }, 1],
  ['unpersisted preparation rejects', s => { fs.unlinkSync(path.join(s.writes[1].dir, 'public/durable-codec.json')); }, 1],
]) test(name, () => {
  const s = specimen();
  try {
    mutate(s);
    fs.writeFileSync(s.casesPath, JSON.stringify(s.cases));
    for (const row of s.writes) fs.writeFileSync(path.join(row.dir, 'public/prepared.json'), JSON.stringify(row.prepared));
    const result = s.run();
    assert.equal(result.status, exit, result.stderr);
    if (!exit) {
      const dir = path.join(s.root, 'output');
      const batch = JSON.parse(fs.readFileSync(path.join(dir, 'batch.json')));
      assert.equal(batch.schema, 'northstar-vrf-sp1-batch-v1'); assert.equal(batch.cases.length, 3);
      const provenance = JSON.parse(fs.readFileSync(path.join(dir, 'provenance.json')));
      assert.equal(provenance.proofs_generated, false); assert.equal(provenance.timing_gate_met, false); assert.equal(provenance.portal_resolutions, 0);
      assert(!JSON.stringify(batch).includes(s.root));
      assert.equal(s.run().status, 1, 'existing output must never be overwritten');
    }
  } finally { fs.rmSync(s.root, { recursive: true, force: true }); }
});
