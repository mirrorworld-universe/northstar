import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';

export const PROGRAM = '5TeWSsjg2gbxCyWVniXeCmwM7UtHTCK7svzJr5xYJzHf';
export const sha256 = (...parts) => createHash('sha256').update(Buffer.concat(parts.map(p => Buffer.from(p)))).digest();
export const hex = bytes => Buffer.from(bytes).toString('hex');
export function bytes(value, length = 32) {
  assert.equal(typeof value, 'string', 'hex string required');
  assert.match(value, new RegExp(`^[0-9a-f]{${length * 2}}$`), 'canonical lowercase hex required');
  return Buffer.from(value, 'hex');
}
export function uint(value, length = 8, endian = 'LE') {
  assert.match(String(value), /^(0|[1-9][0-9]*)$/, 'canonical unsigned integer required');
  const n = BigInt(value);
  assert(n < (1n << BigInt(length * 8)), 'integer overflow');
  const result = Buffer.alloc(length);
  let remaining = n;
  for (let i = 0; i < length; i++) {
    result[endian === 'LE' ? i : length - 1 - i] = Number(remaining & 255n);
    remaining >>= 8n;
  }
  return result;
}
export const instruction = (tag, ...parts) => Buffer.concat([uint(tag, 1), ...parts]);
const tag = Buffer.from('northstar-checkpoint-v1');
const str = s => Buffer.from(s);

export function traceTree(roots) {
  assert(roots.length >= 2 && roots.length <= 17, 'one to sixteen steps required');
  let width = 1;
  while (width < roots.length) width *= 2;
  const leaves = Array.from({ length: width }, (_, i) => i < roots.length
    ? sha256(tag, str('trace'), str('leaf'), uint(36), uint(i, 4), bytes(roots[i]))
    : sha256(tag, str('trace'), str('empty-leaf'), uint(4), uint(i, 4)));
  const layers = [leaves];
  while (layers.at(-1).length > 1) {
    const previous = layers.at(-1);
    layers.push(Array.from({ length: previous.length / 2 }, (_, i) =>
      sha256(tag, str('trace'), str('node'), uint(layers.length - 1, 4), previous[i * 2], previous[i * 2 + 1])));
  }
  return {
    root: sha256(tag, str('trace'), str('root'), uint(roots.length, 4), layers.at(-1)[0]),
    path(index) {
      assert(Number.isInteger(index) && index >= 0 && index < roots.length);
      const path = [];
      for (const layer of layers.slice(0, -1)) {
        path.push(layer[index ^ 1]);
        index = Math.floor(index / 2);
      }
      return { length: path.length, nodes: Buffer.concat([...path, ...Array.from({ length: 5 - path.length }, () => Buffer.alloc(32))]) };
    },
  };
}

export function qualifiedSessionIdentity(session) {
  assert.equal(typeof session.genesis_hash, 'string');
  assert.match(session.genesis_hash, /^[1-9A-HJ-NP-Za-km-z]{32,44}$/);
  const genesis = Buffer.from(session.genesis_hash);
  return hex(sha256(Buffer.from('northstar-local-portal-session-identity-v1'), uint(genesis.length, 4), genesis,
    bytes(session.session_context_preimage_hex, 149)));
}

export function candidateDomain() {
  const domain = Buffer.alloc(32);
  domain.write('northstar-er-step-v1', 8);
  domain[30] = 3;
  domain[31] = 1;
  return domain;
}
const modulus = 21888242871839275222246405745257275088548364400416034343698204186575808495617n;
export function validateBinding(binding, session) {
  assert.equal(binding.schema, 'northstar.vrf-candidate-portal-binding.v1');
  assert.equal(binding.proof_kind, 3);
  assert.equal(binding.proof_version, 1);
  for (const key of ['program_id', 'session', 'grid_id', 'nonce', 'validator', 'session_context_preimage_hex']) {
    assert.equal(binding[key], session[key], `session mismatch: ${key}`);
  }
  assert.equal(binding.program_id, PROGRAM);
  const preimage = bytes(binding.session_context_preimage_hex, 149);
  assert.equal(preimage.subarray(0, 28).toString(), 'northstar-session-context-v1');
  assert.equal(preimage.at(-1), 1);
  const count = binding.state_roots.length - 1;
  assert(count >= 1 && count <= 16, 'bounded checkpoint required');
  assert(Number.isInteger(binding.step_index) && binding.step_index >= 0 && binding.step_index < count);
  uint(binding.er_slot);
  const publicBytes = bytes(binding.public_inputs_hex, 256);
  const fields = Array.from({ length: 8 }, (_, i) => publicBytes.subarray(i * 32, (i + 1) * 32));
  for (const field of fields) assert(BigInt(`0x${hex(field)}`) < modulus, 'noncanonical public field');
  assert.deepEqual(fields[0], candidateDomain());
  assert.deepEqual(fields[2], Buffer.concat([Buffer.alloc(16), uint(binding.er_slot, 8, 'BE'), uint(binding.step_index, 8, 'BE')]));
  const expected = [binding.state_roots[binding.step_index], binding.state_roots[binding.step_index + 1],
    binding.transaction_tree_root, binding.readonly_context_root, binding.settlement_effect_root];
  expected.forEach((value, i) => assert.deepEqual(fields[i + 3], bytes(value)));
  for (const field of [fields[1], ...fields.slice(3)]) assert(!field.equals(Buffer.alloc(32)), 'empty public commitment');
  binding.state_roots.forEach(root => {
    assert(BigInt(`0x${hex(bytes(root))}`) < modulus, 'noncanonical state root');
  });
  assert(binding.captures.length === count, 'one authoritative capture per step required');
  const wires = new Set();
  for (const capture of binding.captures) {
    bytes(capture.capture_sha256);
    bytes(capture.transaction_sha256);
    assert(!wires.has(capture.transaction_sha256), 'duplicate signed transaction');
    wires.add(capture.transaction_sha256);
    assert(['request', 'fulfillment_callback'].includes(capture.role), 'unsupported capture role');
  }
  return { publicBytes, fields, tree: traceTree(binding.state_roots), count };
}

const discriminators = {
  checkpoint: [199, 62, 186, 186, 98, 119, 211, 139],
  challenge: [119, 250, 161, 121, 119, 81, 22, 208],
  da: [24, 139, 155, 40, 135, 132, 79, 129],
  proof: [62, 34, 242, 0, 245, 185, 56, 19],
  cursor: [251, 223, 216, 188, 51, 244, 129, 196],
};
export function decodeState(name, data) {
  const expectedLength = { checkpoint: 380, challenge: 255, da: 178, proof: 668, cursor: 153 }[name];
  assert.equal(data.length, expectedLength, `${name} account length`);
  assert.deepEqual(data.subarray(0, 8), Buffer.from(discriminators[name]));
  const u64 = offset => data.readBigUInt64LE(offset).toString();
  const root = offset => hex(data.subarray(offset, offset + 32));
  switch (name) {
    case 'checkpoint': return { er_slot: u64(40), step_count: u64(48), previous_state_root: root(56), new_state_root: root(88),
      trace_root: root(120), transaction_tree_root: root(152), readonly_context_root: root(184), da_commitment: root(216),
      settlement_effect_root: root(248), proposed_at_slot: u64(312), deadline_slot: u64(320), status: data[328],
      bond_lamports: u64(329), bond_status: data[337], challenge_resolved: data[378] === 1 };
    case 'challenge': return { hard_deadline_slot: u64(112), deadline_slot: u64(120), start_step: u64(128), end_step: u64(136),
      start_state_root: root(152), end_state_root: root(184), status: data[248], turn: data[249], rounds: data.readUInt32LE(250) };
    case 'da': return { commitment: root(72), payload_root: root(104), inclusion_proof_hash: root(136), status: data[176] };
    case 'proof': return { proof_kind: data[104], proof_version: data[105], step_index: u64(106), session_context: root(114),
      transaction_tree_root: root(146), readonly_context_root: root(178), settlement_effect_root: root(210),
      public_input_hash: root(242), written_len: data.readUInt32LE(274), sealed: data[278] === 1,
      proof_hash: root(279), data_hex: hex(data.subarray(312)) };
    case 'cursor': return { active_checkpoint: root(112), active_er_slot: u64(144) };
    default: throw new Error('unsupported account');
  }
}

export function portalComputeUnits(logs) {
  const stack = [];
  const units = [];
  for (const line of logs ?? []) {
    const enter = /^Program (\S+) invoke \[(\d+)\]$/.exec(line);
    if (enter) {
      assert.equal(Number(enter[2]), stack.length + 1, 'incomplete invocation log');
      stack.push(enter[1]);
    }
    const used = new RegExp(`^Program ${PROGRAM} consumed (\\d+) of \\d+ compute units$`).exec(line);
    if (used && stack.length === 1 && stack[0] === PROGRAM) units.push(Number(used[1]));
    const exit = /^Program (\S+) (success|failed:)/.exec(line);
    if (exit && stack.length) assert.equal(stack.pop(), exit[1], 'unbalanced invocation log');
  }
  assert.equal(stack.length, 0, 'truncated invocation log');
  assert.equal(units.length, 1, 'exactly one whole Portal instruction CU record required');
  return units[0];
}
