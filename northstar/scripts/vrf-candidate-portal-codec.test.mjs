import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { PROGRAM, sha256, hex, bytes, uint, instruction, traceTree, candidateDomain, validateBinding, decodeState, portalComputeUnits, qualifiedSessionIdentity } from './vrf-candidate-portal-codec.mjs';

// These are codec-only specimens, never authoritative ER captures or proof evidence.
function specimen() {
  const field = n => hex(uint(n, 32, 'BE'));
  const preimage = Buffer.concat([Buffer.from('northstar-session-context-v1'), Buffer.alloc(120), Buffer.from([1])]);
  const session = { program_id: PROGRAM, session: 'synthetic-codec-session', grid_id: '1', nonce: '0', validator: 'synthetic-codec-validator', session_context_preimage_hex: hex(preimage) };
  const roots = [field(10), field(11), field(12)];
  const binding = { schema: 'northstar.vrf-candidate-portal-binding.v1', ...session, proof_kind: 3, proof_version: 1, er_slot: '37', step_index: 1,
    state_roots: roots, transaction_tree_root: field(13), readonly_context_root: field(14), settlement_effect_root: field(15),
    captures: [0, 1].map(i => ({ capture_sha256: hex(sha256(Buffer.from(`capture${i}`))), transaction_sha256: hex(sha256(Buffer.from(`wire${i}`))), role: i ? 'fulfillment_callback' : 'request' })) };
  binding.public_inputs_hex = hex(Buffer.concat([candidateDomain(), bytes(field(9)), Buffer.concat([Buffer.alloc(16), uint(37, 8, 'BE'), uint(1, 8, 'BE')]), ...[11, 12, 13, 14, 15].map(n => bytes(field(n)))]));
  return { session, binding };
}

test('canonical integer and fixed hex encodings reject truncation', () => {
  assert.equal(hex(uint('18446744073709551615')), 'ffffffffffffffff');
  assert.equal(hex(uint(258, 2)), '0201');
  assert.equal(hex(uint(258, 2, 'BE')), '0102');
  for (const value of [-1, '01', '1.5', '18446744073709551616']) assert.throws(() => uint(value));
  for (const value of ['ff', 'AA'.repeat(32), 'g0'.repeat(32)]) assert.throws(() => bytes(value));
  assert.equal(instruction(21, uint(37), Buffer.from([0])).length, 10);
});

test('candidate public inputs preserve domain/order/slot-step and exact session source', () => {
  const { session, binding } = specimen();
  const validated = validateBinding(binding, session);
  assert.equal(validated.count, 2);
  assert.equal(hex(validated.fields[0]), '00000000000000006e6f727468737461722d65722d737465702d763100000301');
  assert.equal(validated.publicBytes.length, 256);
});

for (const [label, mutate] of [
  ['legacy kind', b => { b.proof_kind = 2; }],
  ['unknown version', b => { b.proof_version = 2; }],
  ['session salt', b => { b.session_context_preimage_hex += '00'; }],
  ['wrong validator', b => { b.validator = 'different'; }],
  ['noncanonical slot', b => { b.er_slot = '037'; }],
  ['wrong selected step', b => { b.step_index = 0; }],
  ['legacy domain', b => { const data = bytes(b.public_inputs_hex, 256); data[30] = 2; b.public_inputs_hex = hex(data); }],
  ['noncanonical field', b => { b.public_inputs_hex = 'ff'.repeat(32) + b.public_inputs_hex.slice(64); }],
  ['wrong root', b => { b.transaction_tree_root = '00'.repeat(31) + '19'; }],
  ['empty commitment', b => { b.public_inputs_hex = b.public_inputs_hex.slice(0, 64) + '00'.repeat(32) + b.public_inputs_hex.slice(128); }],
  ['missing step capture', b => { b.captures.pop(); }],
  ['duplicate signed wire', b => { b.captures[1].transaction_sha256 = b.captures[0].transaction_sha256; }],
  ['unsupported role', b => { b.captures[1].role = 'trace_assertion'; }],
  ['oversized checkpoint', b => { b.state_roots = Array(18).fill(b.state_roots[0]); }],
]) test(`binding guard: ${label}`, () => {
  const { session, binding } = specimen();
  mutate(binding);
  assert.throws(() => validateBinding(binding, session));
});

test('trace paths authenticate every real leaf, with index, level and count binding', () => {
  const { binding } = specimen();
  const roots = [...binding.state_roots, ...binding.state_roots];
  const tree = traceTree(roots);
  const tag = Buffer.from('northstar-checkpoint-v1');
  for (let index = 0; index < roots.length; index++) {
    const auth = tree.path(index);
    assert.equal(auth.length, 3);
    assert(auth.nodes.subarray(auth.length * 32).equals(Buffer.alloc((5 - auth.length) * 32)));
    let current = sha256(tag, Buffer.from('trace'), Buffer.from('leaf'), uint(36), uint(index, 4), bytes(roots[index]));
    let position = index;
    for (let level = 0; level < auth.length; level++) {
      const sibling = auth.nodes.subarray(level * 32, (level + 1) * 32);
      current = sha256(tag, Buffer.from('trace'), Buffer.from('node'), uint(level, 4), ...(position % 2 ? [sibling, current] : [current, sibling]));
      position = Math.floor(position / 2);
    }
    assert.deepEqual(sha256(tag, Buffer.from('trace'), Buffer.from('root'), uint(roots.length, 4), current), tree.root);
  }
  assert.notDeepEqual(traceTree(roots.slice(0, -1)).root, tree.root);
  assert.throws(() => tree.path(roots.length));
});

test('local session identities distinguish actual ledgers without falsifying singleton PDA', () => {
  const { session } = specimen();
  session.genesis_hash = '11111111111111111111111111111111';
  const first = qualifiedSessionIdentity(session);
  const otherLedger = { ...session, genesis_hash: '21111111111111111111111111111111' };
  assert.equal(otherLedger.session, session.session);
  assert.notEqual(qualifiedSessionIdentity(otherLedger), first);
  const context = bytes(session.session_context_preimage_hex, 149);
  context[100] ^= 1;
  assert.notEqual(qualifiedSessionIdentity({ ...session, session_context_preimage_hex: hex(context) }), first);
  assert.equal(qualifiedSessionIdentity({ ...session }), first);
  assert.throws(() => qualifiedSessionIdentity({ ...session, genesis_hash: 'not-a-genesis' }));
});

test('account decoders enforce discriminator and complete Borsh lengths', () => {
  const cases = [['checkpoint', 380, [199, 62, 186, 186, 98, 119, 211, 139]], ['challenge', 255, [119, 250, 161, 121, 119, 81, 22, 208]],
    ['da', 178, [24, 139, 155, 40, 135, 132, 79, 129]], ['proof', 668, [62, 34, 242, 0, 245, 185, 56, 19]], ['cursor', 153, [251, 223, 216, 188, 51, 244, 129, 196]]];
  for (const [name, length, discriminator] of cases) {
    const data = Buffer.alloc(length);
    Buffer.from(discriminator).copy(data);
    assert.doesNotThrow(() => decodeState(name, data));
    assert.throws(() => decodeState(name, data.subarray(1)));
    data[0] ^= 1;
    assert.throws(() => decodeState(name, data));
  }
  const data = Buffer.alloc(668);
  Buffer.from(cases[3][2]).copy(data);
  data[104] = 3; data[105] = 1;
  uint(7).copy(data, 106); uint(356, 4).copy(data, 274); data[278] = 1;
  const decoded = decodeState('proof', data);
  assert.equal(decoded.proof_kind, 3); assert.equal(decoded.proof_version, 1);
  assert.equal(decoded.step_index, '7'); assert.equal(decoded.written_len, 356); assert.equal(decoded.sealed, true);
});

test('CLI refuses host execution, non-loopback endpoints and unknown options before services or wallet creation', () => {
  const script = fileURLToPath(new URL('./vrf-candidate-portal-lifecycle.mjs', import.meta.url));
  for (const [args, environment, expected] of [
    [['init'], { NORTHSTAR_SANDBOX: '0' }, /run inside the Northstar sandbox/],
    [['init', '--rpc', 'https://example.com'], { NORTHSTAR_SANDBOX: '1' }, /loopback development RPC only/],
    [['resolve', '--verifier-mode', 'TestOnly'], { NORTHSTAR_SANDBOX: '1' }, /unknown option/],
  ]) {
    const result = spawnSync(process.execPath, [script, ...args], { env: { ...process.env, ...environment }, encoding: 'utf8' });
    assert.equal(result.status, 1);
    assert.match(result.stderr, expected);
  }
});

test('CU parser measures whole top-level instruction, not verifier marker/CPI/transaction', () => {
  const logs = [`Program ${PROGRAM} invoke [1]`, 'Program log: NS_CU verifier.entry', `Program ${PROGRAM} invoke [2]`,
    `Program ${PROGRAM} consumed 84000 of 100000 compute units`, `Program ${PROGRAM} success`,
    `Program ${PROGRAM} consumed 115326 of 130000 compute units`, `Program ${PROGRAM} success`];
  assert.equal(portalComputeUnits(logs), 115326);
  assert.throws(() => portalComputeUnits(logs.slice(0, -1)));
  assert.throws(() => portalComputeUnits([...logs, ...logs]));
  assert.throws(() => portalComputeUnits([]));
});
