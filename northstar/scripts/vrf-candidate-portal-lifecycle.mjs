#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { PROGRAM, bytes, hex, sha256, uint, instruction, validateBinding, decodeState, portalComputeUnits, qualifiedSessionIdentity } from './vrf-candidate-portal-codec.mjs';

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const stringify = value => JSON.stringify(value, (_, item) => typeof item === 'bigint' ? item.toString() : item, 2) + '\n';
function save(file, value) {
  fs.writeFileSync(file, stringify(value), { flag: 'wx', mode: 0o600 });
}
function load(file) { return JSON.parse(fs.readFileSync(file, 'utf8')); }
function options(argv) {
  const result = {};
  for (let i = 0; i < argv.length; i += 2) {
    assert(argv[i].startsWith('--') && argv[i + 1] && !argv[i + 1].startsWith('--'), 'expected --option value');
    assert(!(argv[i].slice(2) in result), 'duplicate option');
    result[argv[i].slice(2)] = argv[i + 1];
  }
  return result;
}

async function main() {
  assert.equal(process.env.NORTHSTAR_SANDBOX, '1', 'run inside the Northstar sandbox');
  const [mode, ...args] = process.argv.slice(2);
  assert(['init', 'prepare', 'resolve', 'inspect'].includes(mode), 'usage: init|prepare|resolve|inspect --work DIR --rpc http://127.0.0.1:PORT');
  const opts = options(args);
  for (const key of Object.keys(opts)) assert(['work', 'rpc', 'binding', 'binding-validator', 'binding-validator-sha256', 'proof', 'public-inputs', 'grid-id', 'portal-sbf'].includes(key), `unknown option: ${key}`);
  const url = new URL(opts.rpc ?? 'http://127.0.0.1:18999');
  assert(url.protocol === 'http:' && ['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname), 'loopback development RPC only');
  assert(!url.username && !url.password && url.pathname === '/' && !url.search && !url.hash, 'plain loopback endpoint required');
  assert(opts.work && process.env.NORTHSTAR_TS_SDK_DIR, '--work and NORTHSTAR_TS_SDK_DIR required');
  const work = path.resolve(opts.work);
  fs.mkdirSync(work, { recursive: true, mode: 0o700 });
  const publicDir = path.join(work, 'public');
  fs.mkdirSync(publicDir, { recursive: true, mode: 0o700 });
  const receipts = path.join(publicDir, 'receipts');
  fs.mkdirSync(receipts, { recursive: true });
  const require = createRequire(path.join(path.resolve(process.env.NORTHSTAR_TS_SDK_DIR), 'package.json'));
  const { Connection, Keypair, PublicKey, SystemProgram, Transaction, TransactionInstruction, ComputeBudgetProgram } = require('@solana/web3.js');
  const { PortalProgram } = require(path.join(path.resolve(process.env.NORTHSTAR_TS_SDK_DIR), 'dist/programs/portal.js'));
  const rpc = new Connection(url.href, 'confirmed');
  await rpc.getVersion();
  const program = new PublicKey(PROGRAM);
  const derive = (...seeds) => PublicKey.findProgramAddressSync(seeds.map(s => typeof s === 'string' ? Buffer.from(s) : s), program)[0];
  const session = derive('session');
  const vault = derive('fee_vault');
  const cursor = derive('checkpoint_cursor', session.toBuffer());
  const identityPath = path.join(publicDir, 'session.json');
  let ordinal = fs.readdirSync(receipts).filter(name => name.endsWith('.signed.bin')).length;
  const key = name => {
    const keyPath = path.join(work, `${name}.private.json`);
    if (fs.existsSync(keyPath)) return Keypair.fromSecretKey(Uint8Array.from(load(keyPath)));
    assert.equal(mode, 'init', `missing ${name} throwaway wallet`);
    const pair = Keypair.generate();
    save(keyPath, [...pair.secretKey]);
    return pair;
  };
  const validator = key('validator');
  const challenger = key('challenger');
  const meta = (pubkey, isWritable = false, isSigner = false) => ({ pubkey, isWritable, isSigner });
  const portal = (data, keys) => new TransactionInstruction({ programId: program, data, keys });
  async function waitTransaction(signature, timeout = 60000) {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      const response = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'getTransaction', params: [signature, { commitment: 'confirmed', encoding: 'base64', maxSupportedTransactionVersion: 0 }] }), signal: AbortSignal.timeout(5000) });
      assert(response.ok, 'transaction RPC HTTP failure');
      const result = await response.json();
      assert(!result.error, 'transaction RPC error');
      if (result.result?.meta) return result.result;
      await sleep(100);
    }
    throw new Error(`transaction confirmation unavailable: ${signature}; receipts preserve signed attempt`);
  }
  async function send(label, ix, signers = [validator], limit = 300000) {
    const prefix = path.join(receipts, `${String(ordinal++).padStart(3, '0')}-${label}`);
    const latest = await rpc.getLatestBlockhash();
    const tx = new Transaction({ feePayer: validator.publicKey, ...latest }).add(ComputeBudgetProgram.setComputeUnitLimit({ units: limit }), ix);
    tx.sign(...signers);
    const wire = tx.serialize();
    fs.writeFileSync(`${prefix}.signed.bin`, wire, { flag: 'wx', mode: 0o600 });
    save(`${prefix}.instructions.json`, { label, transaction_sha256: hex(sha256(wire)), ordered_instructions: tx.instructions.map(i => ({ program: i.programId.toBase58(), data_hex: hex(i.data), accounts: i.keys.map(k => ({ key: k.pubkey.toBase58(), signer: k.isSigner, writable: k.isWritable })) })) });
    let signature;
    try {
      signature = await rpc.sendRawTransaction(wire, { skipPreflight: true, maxRetries: 3 });
      save(`${prefix}.submitted.json`, { signature, latest_blockhash: latest, submitted_at: new Date().toISOString() });
      const result = await waitTransaction(signature);
      save(`${prefix}.rpc.json`, result);
      assert.equal(result.transaction[1], 'base64');
      assert.deepEqual(Buffer.from(result.transaction[0], 'base64'), wire, 'RPC transaction differs from signed submission');
      let units = null;
      let cuError;
      try { units = portalComputeUnits(result.meta.logMessages); } catch (error) { cuError = String(error); }
      save(`${prefix}.outcome.json`, { signature, slot: result.slot, error: result.meta.err, portal_instruction_cu: units, cu_measurement_error: cuError, transaction_cu: result.meta.computeUnitsConsumed });
      assert.equal(result.meta.err, null, `${label} transaction failed; full receipt retained`);
      assert(units !== null, 'whole Portal instruction CU unavailable; full logs retained');
      return { signature, slot: result.slot, units };
    } catch (error) {
      save(`${prefix}.error.json`, { signature, error: String(error) });
      throw error;
    }
  }
  async function account(pubkey) {
    const result = await rpc.getAccountInfo(pubkey);
    assert(result && result.owner.equals(program), `missing/wrong-owner Portal account: ${pubkey}`);
    return result;
  }
  async function sessionIdentity() {
    const raw = (await account(session)).data;
    const state = PortalProgram.parseSession(raw);
    assert(state.validator.equals(validator.publicKey) && state.authority.equals(validator.publicKey), 'session is not owned by this fresh development run');
    const preimage = Buffer.concat([Buffer.from('northstar-session-context-v1'), program.toBuffer(), session.toBuffer(), uint(state.gridId), uint(state.nonce, 16), state.validator.toBuffer(), Buffer.from([1])]);
    assert.equal(preimage.length, 149);
    return { schema: 'northstar.vrf-candidate-portal-session.v1', program_id: PROGRAM, session: session.toBase58(), grid_id: state.gridId.toString(), nonce: state.nonce.toString(), validator: state.validator.toBase58(), challenger: challenger.publicKey.toBase58(), session_context_preimage_hex: hex(preimage), session_account_sha256: hex(sha256(raw)), created_at_slot: state.createdAt.toString(), ttl_slots: state.ttlSlots.toString(), genesis_hash: await rpc.getGenesisHash() };
  }
  async function checkIdentity() {
    const expected = load(identityPath);
    const actual = await sessionIdentity();
    assert.deepEqual(actual, expected, 'session/genesis changed between phases');
    const configured = load(path.join(publicDir, 'portal-program.json'));
    await checkProgram(configured);
    return actual;
  }
  async function checkProgram(expected) {
    assert(opts['portal-sbf'], 'explicit --portal-sbf required in every phase');
    const elf = fs.readFileSync(opts['portal-sbf']);
    const onchain = await rpc.getAccountInfo(program);
    assert(onchain?.executable, 'Portal must be an actual deployed executable');
    let onchainElf;
    if (onchain.owner.toBase58() === 'BPFLoaderUpgradeab1e11111111111111111111111') {
      assert.equal(onchain.data.readUInt32LE(0), 2);
      const data = await rpc.getAccountInfo(new PublicKey(onchain.data.subarray(4, 36)));
      assert(data && data.owner.equals(onchain.owner) && data.data.readUInt32LE(0) === 3);
      onchainElf = data.data.subarray(45, 45 + elf.length);
    } else {
      assert(['BPFLoader1111111111111111111111111111111111', 'BPFLoader2111111111111111111111111111111111'].includes(onchain.owner.toBase58()), 'unsupported Portal loader');
      onchainElf = onchain.data;
    }
    assert.deepEqual(onchainElf, elf, 'deployed Portal differs from supplied SBF');
    const receipt = { schema: 'northstar.vrf-candidate-portal-program.v1', program_id: PROGRAM, loader: onchain.owner.toBase58(), elf_sha256: hex(sha256(elf)) };
    if (expected) assert.deepEqual(receipt, expected);
    return receipt;
  }

  if (mode === 'init') {
    assert(!fs.existsSync(identityPath), 'init requires a new lifecycle work directory');
    assert.equal(await rpc.getAccountInfo(session), null, 'init requires a fresh Portal session/ledger');
    const programReceipt = await checkProgram();
    save(path.join(publicDir, 'portal-program.json'), programReceipt);
    for (const pair of [validator, challenger]) {
      const signature = await rpc.requestAirdrop(pair.publicKey, 2000000000);
      const result = await waitTransaction(signature);
      save(path.join(receipts, `fund-${pair === validator ? 'validator' : 'challenger'}.json`), result);
      assert.equal(result.meta.err, null);
    }
    await send('open-session', portal(Buffer.from(PortalProgram.encodeOpenSession({ gridId: BigInt(opts['grid-id'] ?? '1'), ttlSlots: 1000000n, feeCap: 1000000000n, validator: validator.publicKey, settlementIntervalSlots: 10n })), [meta(validator.publicKey, true, true), meta(session, true), meta(vault, true), meta(SystemProgram.programId)]));
    save(identityPath, await sessionIdentity());
    console.log('Session opened; candidate execution/proof/acceptance not established. Export public/session.json to canonical binder.');
    return;
  }
  const identity = await checkIdentity();
  if (mode === 'inspect') {
    console.log(stringify(identity));
    return;
  }
  assert(opts.binding, '--binding canonical candidate manifest required');
  const bindingFile = path.resolve(opts.binding);
  const bindingRaw = fs.readFileSync(bindingFile);
  const binding = JSON.parse(bindingRaw);
  const { publicBytes, fields, tree, count } = validateBinding(binding, identity);
  assert(opts['binding-validator'] && opts['binding-validator-sha256'], 'UNCONFIGURED: actual interpreter binding validator and reviewed SHA-256 required');
  const binder = path.resolve(opts['binding-validator']);
  assert.equal(hex(sha256(fs.readFileSync(binder))), hex(bytes(opts['binding-validator-sha256'])), 'binding validator identity mismatch');
  const checked = spawnSync(binder, [bindingFile, identityPath], { encoding: 'utf8', timeout: 300000, maxBuffer: 1024 * 1024 });
  const auditName = `${mode}-binding-${Date.now()}`;
  save(path.join(work, `${auditName}.private.json`), { status: checked.status, signal: checked.signal, error: checked.error?.message, stdout: checked.stdout, stderr: checked.stderr });
  save(path.join(publicDir, `${auditName}.json`), { binding_sha256: hex(sha256(bindingRaw)), validator_sha256: opts['binding-validator-sha256'], status: checked.status, signal: checked.signal });
  assert.equal(checked.status, 0, 'canonical interpreter binding validation failed');
  const validation = JSON.parse(checked.stdout);
  assert.equal(validation.schema, 'northstar.vrf-candidate-portal-binding-validation.v1');
  assert.equal(validation.binding_sha256, hex(sha256(bindingRaw)));
  assert.equal(validation.public_inputs_sha256, hex(sha256(publicBytes)));
  assert.equal(validation.execution_verified, true, 'binding-only/trace-only adapters are not supported');
  assert.deepEqual(validation.capture_sha256, binding.captures.map(c => c.capture_sha256));

  const er = uint(binding.er_slot);
  const checkpoint = derive('checkpoint', session.toBuffer(), er);
  const challenge = derive('challenge', checkpoint.toBuffer());
  const da = derive('da_proof', challenge.toBuffer());
  const proof = derive('step_proof', checkpoint.toBuffer());
  const keys = { checkpoint, challenge, da, proof, cursor };
  async function state(name) { return decodeState(name, (await account(keys[name])).data); }
  async function snapshot(label) {
    const entries = Object.entries({ session, ...keys, validator: validator.publicKey, challenger: challenger.publicKey });
    const result = await rpc.getMultipleAccountsInfoAndContext(entries.map(([, key]) => key));
    const output = { slot: result.context.slot, accounts: result.value.map((a, i) => ({ role: entries[i][0], address: entries[i][1].toBase58(), account: a ? { lamports: a.lamports, owner: a.owner.toBase58(), executable: a.executable, rent_epoch: a.rentEpoch, data_base64: a.data.toString('base64') } : null })) };
    save(path.join(publicDir, `${label}-${Date.now()}.accounts.json`), output);
    return output;
  }
  function checkProofMetadata(p) {
    assert.equal(p.proof_kind, 3); assert.equal(p.proof_version, 1);
    for (const [name, field] of [['session_context', 1], ['transaction_tree_root', 5], ['readonly_context_root', 6], ['settlement_effect_root', 7]]) assert.equal(p[name], hex(fields[field]));
    assert.equal(p.step_index, String(binding.step_index));
    const expected = sha256(Buffer.from('northstar-er-step-v1'), program.toBuffer(), session.toBuffer(), checkpoint.toBuffer(),
      Buffer.from([3]), Buffer.from([1]), er, uint(binding.step_index), fields[1], fields[3], fields[4], fields[5], fields[6], fields[7]);
    assert.equal(p.public_input_hash, hex(expected), 'Portal metadata binding digest mismatch');
  }
  async function liveTurn() {
    const s = await state('challenge');
    const slot = BigInt(await rpc.getSlot());
    assert(slot < BigInt(s.deadline_slot) && slot < BigInt(s.hard_deadline_slot), 'challenge expired: create a new fresh run; never inject/extend state');
    return s;
  }
  const preparedPath = path.join(publicDir, 'prepared.json');
  if (fs.existsSync(preparedPath)) assert.equal(load(preparedPath).binding_sha256, hex(sha256(bindingRaw)), 'different binding in prepared lifecycle');
  await snapshot(`${mode}-entry`);
  try {
  if (mode === 'prepare') {
    const artifacts = binding.captures.map(c => {
      const file = path.resolve(path.dirname(bindingFile), c.capture_file);
      const raw = fs.readFileSync(file);
      assert.equal(hex(sha256(raw)), c.capture_sha256, 'capture artifact changed');
      const wire = fs.readFileSync(path.resolve(path.dirname(bindingFile), c.transaction_file));
      assert.equal(hex(sha256(wire)), c.transaction_sha256, 'signed transaction artifact changed');
      return raw;
    });
    const daCommitment = sha256(Buffer.from('northstar-vrf-candidate-capture-da-v1'), ...artifacts.map(a => Buffer.concat([uint(a.length), a])));
    const daInclusion = sha256(Buffer.from('northstar-vrf-candidate-capture-da-reveal-v1'), daCommitment, ...binding.captures.map(c => bytes(c.capture_sha256)));
    if (!(await rpc.getAccountInfo(checkpoint))) {
      await send('propose-checkpoint-bond', portal(instruction(14, er, uint(count), bytes(binding.state_roots[0]), bytes(binding.state_roots.at(-1)), tree.root, fields[5], fields[6], daCommitment, fields[7], uint(9000)), [meta(validator.publicKey, true, true), meta(session), meta(checkpoint, true), meta(cursor, true), meta(SystemProgram.programId)]));
    }
    let c = await state('checkpoint');
    for (const [key, value] of Object.entries({ er_slot: binding.er_slot, step_count: String(count), previous_state_root: binding.state_roots[0], new_state_root: binding.state_roots.at(-1), trace_root: hex(tree.root), transaction_tree_root: binding.transaction_tree_root, readonly_context_root: binding.readonly_context_root, settlement_effect_root: binding.settlement_effect_root, da_commitment: hex(daCommitment) })) assert.equal(c[key], value, `checkpoint ${key}`);
    assert.equal(c.bond_status, 0);
    assert.equal(c.bond_lamports, '1000000');
    if (!(await rpc.getAccountInfo(challenge))) {
      await send('open-challenge', portal(instruction(17, er), [meta(challenger.publicKey, true, true), meta(session), meta(checkpoint, true), meta(challenge, true), meta(da, true), meta(SystemProgram.programId)]), [validator, challenger]);
    }
    while (true) {
      const s = await liveTurn();
      assert.equal(s.status, 0);
      if (s.turn === 2) break;
      const start = Number(s.start_step), end = Number(s.end_step);
      if (s.turn === 0) {
        const width = end - start;
        const selected = width === 1 ? start : start + Math.floor(width / 2);
        const root = bytes(binding.state_roots[width === 1 ? end : selected]);
        const auth = width === 1 ? { length: 0, nodes: Buffer.alloc(160) } : tree.path(selected);
        await send(`respond-${start}-${end}`, portal(instruction(23, er, uint(selected), root, uint(auth.length, 1), auth.nodes, daCommitment, daInclusion), [meta(validator.publicKey, false, true), meta(session), meta(checkpoint), meta(challenge, true), meta(da, true)]));
      } else {
        assert.equal(s.turn, 1);
        const midpoint = start + Math.floor((end - start) / 2);
        await send(`bisect-${start}-${end}`, portal(instruction(24, er, uint(binding.step_index >= midpoint ? 1 : 0, 1)), [meta(challenger.publicKey, false, true), meta(session), meta(checkpoint), meta(challenge, true)]), [validator, challenger]);
      }
    }
    const isolated = await liveTurn();
    assert.equal(isolated.start_step, String(binding.step_index));
    assert.equal(isolated.end_step, String(binding.step_index + 1));
    assert.equal(isolated.start_state_root, binding.state_roots[binding.step_index]);
    assert.equal(isolated.end_state_root, binding.state_roots[binding.step_index + 1]);
    assert.equal((await state('da')).status, 1);
    if (!(await rpc.getAccountInfo(proof))) {
      await send('create-kind3-proof', portal(instruction(18, er, Buffer.from([3, 1]), uint(binding.step_index), fields[1], fields[5], Buffer.alloc(1 + 128), fields[6], fields[7]), [meta(challenger.publicKey, true, true), meta(session), meta(checkpoint), meta(challenge), meta(proof, true), meta(SystemProgram.programId)]), [validator, challenger]);
    }
    checkProofMetadata(await state('proof'));
    const preparation = await snapshot('prepared');
    if (!fs.existsSync(preparedPath)) save(preparedPath, { schema: 'northstar.vrf-candidate-portal-prepared.v1', binding_sha256: hex(sha256(bindingRaw)), public_inputs_sha256: hex(sha256(publicBytes)), session: identity, session_source_sha256: hex(sha256(fs.readFileSync(identityPath))), session_identity_sha256: qualifiedSessionIdentity(identity), portal_program: load(path.join(publicDir, 'portal-program.json')), er_slot: binding.er_slot, step_index: binding.step_index, addresses: Object.fromEntries(Object.entries(keys).map(([k, v]) => [k, v.toBase58()])), deadline_slot: isolated.deadline_slot, prepared_slot: preparation.slot, kind: 3, version: 1 });
    console.log('Fresh challenge isolated and kind3 proof metadata created. Proof verification and acceptance remain pending.');
    return;
  }
  assert(fs.existsSync(preparedPath), 'run prepare before resolve');
  assert(opts.proof && opts['public-inputs'], '--proof and --public-inputs required');
  const proofBytes = fs.readFileSync(opts.proof);
  assert.equal(proofBytes.length, 356, 'actual SP1 on-chain envelope required');
  assert.deepEqual(fs.readFileSync(opts['public-inputs']), publicBytes, 'proof public inputs differ from prepared canonical binding');
  await liveTurn();
  const p = await state('proof');
  checkProofMetadata(p);
  if (!p.sealed) {
    assert.equal(p.data_hex.slice(0, p.written_len * 2), hex(proofBytes.subarray(0, p.written_len)), 'partial upload differs from supplied proof');
    for (let offset = p.written_len; offset < proofBytes.length; offset += 128) {
      const chunk = proofBytes.subarray(offset, offset + 128);
      await send(`write-proof-${offset}`, portal(instruction(19, er, uint(offset, 4), uint(chunk.length, 2), Buffer.concat([chunk, Buffer.alloc(128 - chunk.length)])), [meta(challenger.publicKey, false, true), meta(session), meta(checkpoint), meta(challenge), meta(proof, true)]), [validator, challenger]);
    }
    await send('seal-proof', portal(instruction(20, er, uint(356, 4)), [meta(challenger.publicKey, false, true), meta(session), meta(checkpoint), meta(challenge), meta(proof, true)]), [validator, challenger]);
  }
  const sealed = await state('proof');
  assert.equal(sealed.data_hex, hex(proofBytes));
  assert.equal(sealed.proof_hash, hex(sha256(proofBytes)));
  assert.equal(sealed.written_len, 356); assert.equal(sealed.sealed, true);
  const before = await snapshot('before-resolve');
  const result = await send('resolve-production', portal(instruction(21, er, Buffer.from([0])), [meta(validator.publicKey, false, true), meta(session), meta(checkpoint, true), meta(challenge, true), meta(da), meta(proof), meta(challenger.publicKey, true), meta(cursor, true)]), [validator], 130000);
  const after = await snapshot('after-resolve');
  const checkpointState = await state('checkpoint');
  const challengeState = await state('challenge');
  save(path.join(publicDir, `resolution-${Date.now()}.json`), { schema: 'northstar.vrf-candidate-portal-resolution.v1', binding_sha256: hex(sha256(bindingRaw)), proof_sha256: hex(sha256(proofBytes)), public_inputs_sha256: hex(sha256(publicBytes)), ...result, before_slot: before.slot, after_slot: after.slot, checkpoint: checkpointState, challenge: challengeState, production: true, verification_outcome: checkpointState.status === 0 && challengeState.status === 2 ? 'valid' : 'not_valid', hosted_acceptance: false });
  assert(result.units <= 130000, 'whole Production resolution exceeds 130000 CU');
  assert.equal(checkpointState.status, 0, 'proof was not valid: Production may successfully resolve invalid proofs by slashing');
  assert.equal(checkpointState.challenge_resolved, true);
  assert.equal(checkpointState.bond_status, 0, 'valid resolution must preserve locked bond until checkpoint commitment');
  assert.equal(challengeState.status, 2, 'validator did not win actual Production verification');
  assert.equal((await state('cursor')).active_checkpoint, hex(checkpoint.toBuffer()));
  const beforeCheckpoint = before.accounts.find(a => a.role === 'checkpoint').account;
  const afterCheckpoint = after.accounts.find(a => a.role === 'checkpoint').account;
  assert.equal(afterCheckpoint.lamports, beforeCheckpoint.lamports, 'valid resolution changed checkpoint bond balance');
  assert.equal(after.accounts.find(a => a.role === 'challenger').account.lamports, before.accounts.find(a => a.role === 'challenger').account.lamports, 'valid resolution paid challenger');
  console.log(`Actual kind3 Production proof valid; whole Portal instruction ${result.units} CU. Local development only.`);
  } catch (error) {
    await snapshot(`${mode}-failed`);
    throw error;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(String(error)); process.exitCode = 1; });
}
