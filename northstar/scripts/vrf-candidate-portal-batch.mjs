#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { bytes, hex, sha256, validateBinding, qualifiedSessionIdentity } from './vrf-candidate-portal-codec.mjs';

try {
  assert.equal(process.argv.length, 4, 'usage: vrf-candidate-portal-batch.mjs LOCAL_CASES_JSON FRESH_OUTPUT_DIR');
  const source = path.resolve(process.argv[2]);
  const root = path.dirname(source);
  const cases = JSON.parse(fs.readFileSync(source));
  assert.equal(cases.schema, 'northstar.vrf-candidate-portal-cases.v1');
  assert.equal(cases.cases.length, 3, 'exactly three prepared cases required');
  const validator = process.env.NORTHSTAR_VRF_BINDING_VALIDATOR;
  const validatorHash = process.env.NORTHSTAR_VRF_BINDING_VALIDATOR_SHA256;
  assert(validator && validatorHash, 'immutable actual native binding validator and reviewed digest required');
  assert.equal(hex(sha256(fs.readFileSync(validator))), hex(bytes(validatorHash)));
  const output = path.resolve(process.argv[3]);
  fs.mkdirSync(output, { mode: 0o700 });
  const identities = new Set(), inputs = new Set(), publics = new Set(), ids = new Set();
  const roles = new Set();
  const batch = { schema: 'northstar-vrf-sp1-batch-v1', warmup: null, cases: [] };
  const provenance = [];
  const write = (file, content) => fs.writeFileSync(path.join(output, file), content, { flag: 'wx', mode: 0o600 });
  for (const entry of cases.cases) {
    assert.equal(entry.status, 'prepared', 'case is not actually prepared');
    assert.match(entry.case_id, /^[a-zA-Z0-9_-]{1,64}$/);
    assert(!ids.has(entry.case_id), 'duplicate case id'); ids.add(entry.case_id);
    const work = path.resolve(root, entry.work);
    const sessionFile = path.join(work, 'public/session.json');
    const sessionRaw = fs.readFileSync(sessionFile);
    const session = JSON.parse(sessionRaw);
    const prepareRaw = fs.readFileSync(path.join(work, 'public/prepared.json'));
    const prepared = JSON.parse(prepareRaw);
    assert.equal(prepared.schema, 'northstar.vrf-candidate-portal-prepared.v1');
    assert.equal(prepared.kind, 3); assert.equal(prepared.version, 1);
    assert.deepEqual(prepared.session, session);
    assert.equal(prepared.session_source_sha256, hex(sha256(sessionRaw)));
    const identity = qualifiedSessionIdentity(session);
    assert.equal(prepared.session_identity_sha256, identity);
    assert(!identities.has(identity), 'duplicate actual ledger/session context'); identities.add(identity);
    assert.equal(prepared.portal_program.elf_sha256, cases.portal_sbf_sha256, 'configured deployment changed');
    assert(BigInt(prepared.deadline_slot) > 0n);
    assert(Number.isSafeInteger(prepared.prepared_slot) && prepared.prepared_slot > 0, 'missing actual preparation slot');
    const fences = fs.readdirSync(path.join(work, 'public')).filter(name => /^durable-.*\.json$/.test(name))
      .map(name => JSON.parse(fs.readFileSync(path.join(work, 'public', name))));
    assert(fences.some(f => f.schema === 'northstar.vrf-candidate-portal-durable.v1' && f.phase === 'prepare' && f.phase_exit === 0
      && f.confirmed_fence >= prepared.prepared_slot && f.snapshot_slot >= f.confirmed_fence), 'no successful durable snapshot covers actual preparation');
    const bindingFile = path.resolve(root, entry.binding_json);
    const bindingRaw = fs.readFileSync(bindingFile);
    const binding = JSON.parse(bindingRaw);
    assert.equal(binding.profile_sha256, cases.policy_sha256, 'candidate policy changed');
    const checked = validateBinding(binding, session);
    assert.equal(prepared.binding_sha256, hex(sha256(bindingRaw)));
    assert.equal(prepared.public_inputs_sha256, hex(sha256(checked.publicBytes)));
    assert.equal(prepared.er_slot, binding.er_slot); assert.equal(prepared.step_index, binding.step_index);
    const result = spawnSync(validator, [bindingFile, sessionFile], { encoding: 'utf8', timeout: 300000, maxBuffer: 1024 * 1024 });
    assert.equal(result.status, 0, `actual replay/binding rejected case ${entry.case_id}; no batch acceptance claimed`);
    const validation = JSON.parse(result.stdout);
    assert.equal(validation.schema, 'northstar.vrf-candidate-portal-binding-validation.v1');
    assert.equal(validation.execution_verified, true);
    assert.equal(validation.binding_sha256, prepared.binding_sha256);
    assert.equal(validation.public_inputs_sha256, prepared.public_inputs_sha256);
    assert.deepEqual(validation.capture_sha256, binding.captures.map(c => c.capture_sha256));
    binding.captures.forEach(c => roles.add(c.role));
    const input = fs.readFileSync(path.resolve(path.dirname(bindingFile), binding.witness_file));
    const publicBytes = fs.readFileSync(path.resolve(path.dirname(bindingFile), binding.public_inputs_file));
    assert.deepEqual(publicBytes, checked.publicBytes);
    const inputHash = hex(sha256(input));
    assert(!inputs.has(inputHash), 'duplicate envelope'); inputs.add(inputHash);
    assert(!publics.has(prepared.public_inputs_sha256), 'duplicate public inputs'); publics.add(prepared.public_inputs_sha256);
    fs.mkdirSync(path.join(output, entry.case_id));
    write(`${entry.case_id}/witness.borsh`, input);
    write(`${entry.case_id}/public-inputs.bin`, publicBytes);
    write(`${entry.case_id}/prepared.json`, prepareRaw);
    batch.cases.push({ case_id: entry.case_id, input: `${entry.case_id}/witness.borsh`, input_sha256: inputHash,
      expected_public: `${entry.case_id}/public-inputs.bin`, public_inputs_sha256: prepared.public_inputs_sha256,
      prepare_record: `${entry.case_id}/prepared.json`, prepare_record_sha256: hex(sha256(prepareRaw)) });
    provenance.push({ case_id: entry.case_id, session_identity_sha256: identity, session_source_sha256: prepared.session_source_sha256,
      binding_sha256: prepared.binding_sha256, binder_sha256: validatorHash, captures: binding.captures.map(({ role, capture_sha256, transaction_sha256 }) => ({ role, capture_sha256, transaction_sha256 })) });
  }
  assert(roles.has('request') && roles.has('fulfillment_callback'), 'window requires actual request and atomic fulfillment/callback');
  write('batch.json', JSON.stringify(batch, null, 2) + '\n');
  write('provenance.json', JSON.stringify({ schema: 'northstar.vrf-candidate-portal-batch-provenance.v1',
    portal_sbf_sha256: cases.portal_sbf_sha256, program_vkey_hash: cases.program_vkey_hash, policy_sha256: cases.policy_sha256, cases: provenance, requires_qualified_identity_client: true, proofs_generated: false, portal_resolutions: 0, timing_gate_met: false }, null, 2) + '\n');
  console.log(JSON.stringify({ schema: 'northstar.vrf-candidate-portal-batch-ready.v1', batch_sha256: hex(sha256(fs.readFileSync(path.join(output, 'batch.json')))), cases: batch.cases.length, requires_qualified_identity_client: true, proofs_generated: false }));
} catch (error) {
  console.error(String(error));
  process.exitCode = 1;
}
