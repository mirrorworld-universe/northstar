import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { PROGRAM, hex, sha256, instruction, uint } from './vrf-candidate-portal-codec.mjs';

const sdk = process.env.NORTHSTAR_TS_SDK_DIR;
const script = fileURLToPath(new URL('./vrf-candidate-portal-receipts.mjs', import.meta.url));

// Signed synthetic codec specimens have no bank, execution, witness, or proof.
function specimen(mode = 0) {
  const require = createRequire(path.join(path.resolve(sdk), 'package.json'));
  const { Transaction, Keypair, PublicKey, TransactionInstruction } = require('@solana/web3.js');
  const pair = Keypair.generate();
  const tx = new Transaction({ feePayer: pair.publicKey, recentBlockhash: '11111111111111111111111111111111' }).add(
    new TransactionInstruction({ programId: new PublicKey(PROGRAM), keys: [{ pubkey: pair.publicKey, isWritable: true, isSigner: true }], data: instruction(21, uint(37), uint(mode, 1)) }));
  tx.sign(pair);
  const wire = tx.serialize();
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'vrf-portal-receipt-codec-'));
  const receipts = path.join(temp, 'receipts');
  fs.mkdirSync(receipts);
  const stem = path.join(receipts, '000-resolve-production');
  const bs58Module = require('bs58');
  const signature = (bs58Module.default ?? bs58Module).encode(tx.signature);
  const files = {
    rpc: { slot: 10, transaction: [wire.toString('base64'), 'base64'], meta: { err: null, computeUnitsConsumed: 115476,
      logMessages: [`Program ${PROGRAM} invoke [1]`, `Program ${PROGRAM} consumed 115326 of 130000 compute units`, `Program ${PROGRAM} success`] } },
    instructions: { transaction_sha256: hex(sha256(wire)), ordered_instructions: tx.instructions.map(ix => ({ program: ix.programId.toBase58(), data_hex: hex(ix.data), accounts: ix.keys.map(k => ({ key: k.pubkey.toBase58(), signer: k.isSigner, writable: k.isWritable })) })) },
    outcome: { signature, slot: 10, error: null, portal_instruction_cu: 115326, transaction_cu: 115476 },
  };
  const write = () => {
    fs.writeFileSync(`${stem}.signed.bin`, wire);
    for (const [name, content] of Object.entries(files)) fs.writeFileSync(`${stem}.${name}.json`, JSON.stringify(content));
  };
  write();
  return { temp, stem, wire, files, write };
}

for (const [name, mutate, expected] of [
  ['consistent signed synthetic receipt stays explicitly not accepted', () => {}, 0],
  ['changed whole-instruction CU is rejected', s => { s.files.outcome.portal_instruction_cu--; s.write(); }, 1],
  ['changed returned wire is rejected', s => { s.files.rpc.transaction[0] = Buffer.from('different').toString('base64'); s.write(); }, 1],
  ['changed instruction identity is rejected', s => { s.files.instructions.ordered_instructions[0].data_hex = '00'; s.write(); }, 1],
  ['changed signature is rejected', s => { const wire = Buffer.from(s.wire); wire[1] ^= 1; fs.writeFileSync(`${s.stem}.signed.bin`, wire); }, 1],
  ['missing confirmation is preserved, not claimed accepted', s => { fs.unlinkSync(`${s.stem}.rpc.json`); }, 0],
]) test(name, { skip: !sdk }, () => {
  const s = specimen();
  try {
    mutate(s);
    const result = spawnSync(process.execPath, [script, s.temp], { encoding: 'utf8' });
    assert.equal(result.status, expected, result.stderr);
    if (!expected) {
      const audit = JSON.parse(result.stdout);
      assert.equal(audit.candidate_acceptance, 'not_established');
      if (name.startsWith('missing')) assert.equal(audit.failed_or_unconfirmed_attempts, 1);
    }
  } finally { fs.rmSync(s.temp, { recursive: true, force: true }); }
});

test('TestOnly resolution is rejected even with a valid synthetic signature', { skip: !sdk }, () => {
  const s = specimen(1);
  try {
    const result = spawnSync(process.execPath, [script, s.temp], { encoding: 'utf8' });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /only Production resolution is permitted/);
  } finally { fs.rmSync(s.temp, { recursive: true, force: true }); }
});
