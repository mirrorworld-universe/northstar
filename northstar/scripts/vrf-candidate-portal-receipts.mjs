#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { PROGRAM, hex, sha256, portalComputeUnits } from './vrf-candidate-portal-codec.mjs';

try {
  assert.equal(process.argv.length, 3, 'usage: vrf-candidate-portal-receipts.mjs PUBLIC_RECEIPT_DIR');
  assert(process.env.NORTHSTAR_TS_SDK_DIR, 'read-only NORTHSTAR_TS_SDK_DIR required');
  const require = createRequire(path.join(path.resolve(process.env.NORTHSTAR_TS_SDK_DIR), 'package.json'));
  const { Transaction } = require('@solana/web3.js');
  const bs58Module = require('bs58');
  const bs58 = bs58Module.default ?? bs58Module;
  const directory = path.resolve(process.argv[2]);
  const receipts = path.join(directory, 'receipts');
  const read = file => JSON.parse(fs.readFileSync(file, 'utf8'));
  const names = fs.readdirSync(receipts).filter(name => name.endsWith('.signed.bin')).sort();
  assert(names.length, 'no signed Portal receipts');
  const samples = [];
  let failed = 0;
  for (const name of names) {
    const stem = name.slice(0, -'.signed.bin'.length);
    const wire = fs.readFileSync(path.join(receipts, name));
    const tx = Transaction.from(wire);
    assert(tx.verifySignatures(), 'invalid signed receipt');
    const signature = bs58.encode(tx.signature);
    const resultFile = path.join(receipts, `${stem}.rpc.json`);
    if (!fs.existsSync(resultFile)) {
      failed++;
      samples.push({ receipt: stem, signature, transaction_sha256: hex(sha256(wire)), classification: 'submission_without_confirmed_rpc_receipt' });
      continue;
    }
    const result = read(resultFile);
    assert.equal(result.transaction[1], 'base64', 'raw base64 RPC receipt required');
    assert.deepEqual(Buffer.from(result.transaction[0], 'base64'), wire);
    const instructions = read(path.join(receipts, `${stem}.instructions.json`));
    assert.equal(instructions.transaction_sha256, hex(sha256(wire)));
    assert.equal(instructions.ordered_instructions.length, tx.instructions.length);
    tx.instructions.forEach((ix, i) => {
      const recorded = instructions.ordered_instructions[i];
      assert.equal(recorded.program, ix.programId.toBase58());
      assert.equal(recorded.data_hex, hex(ix.data));
      assert.deepEqual(recorded.accounts.map(a => a.key), ix.keys.map(a => a.pubkey.toBase58()));
    });
    assert.equal(tx.instructions.filter(ix => ix.programId.toBase58() === PROGRAM).length, 1, 'exactly one Portal instruction required');
    const actualError = result.meta.err;
    let units = null;
    try { units = portalComputeUnits(result.meta.logMessages); } catch (error) { assert(actualError, String(error)); }
    const outcome = read(path.join(receipts, `${stem}.outcome.json`));
    assert.equal(outcome.signature, signature);
    assert.deepEqual(outcome.error, actualError);
    assert.equal(outcome.slot, result.slot);
    assert.equal(outcome.portal_instruction_cu, units);
    assert.equal(outcome.transaction_cu, result.meta.computeUnitsConsumed);
    if (actualError) failed++;
    const portal = tx.instructions.find(ix => ix.programId.toBase58() === PROGRAM);
    if (portal.data[0] === 21) {
      assert.equal(portal.data.length, 10);
      assert.equal(portal.data[9], 0, 'only Production resolution is permitted');
    }
    samples.push({ receipt: stem, signature, slot: result.slot, transaction_sha256: hex(sha256(wire)), rpc_receipt_sha256: hex(sha256(fs.readFileSync(resultFile))),
      portal_tag: portal.data[0], portal_instruction_cu: units, transaction_cu: result.meta.computeUnitsConsumed, transaction_error: actualError });
  }
  console.log(JSON.stringify({ schema: 'northstar.vrf-candidate-portal-receipt-audit.v1', classification: 'offline_structural_audit_not_consensus_or_proof_verification',
    signed_attempts: names.length, failed_or_unconfirmed_attempts: failed, samples, candidate_acceptance: 'not_established' }, null, 2));
} catch (error) {
  console.error(String(error));
  process.exitCode = 1;
}
