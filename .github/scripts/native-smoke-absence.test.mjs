import { test, expect } from 'bun:test';
import { assertAbsenceWitness } from './native-smoke-absence.mjs';

const witness = () => ({ blockhash_valid: false, status: null, transaction: null,
  current_height: 200, expiry_height: 100,
  balances: { processed: 10_000_000, confirmed: 10_000_000, finalized: 10_000_000 },
  recipient_l1_balance: 0, receipt_total: '10000000', receipt_withdrawn: '0',
  context: '01'.repeat(32), expected_context: '01'.repeat(32) });

test('terminal absence requires expired lifetime and preserved native withdrawal state', () => {
  expect(() => assertAbsenceWitness(witness())).not.toThrow();
});

test('fresh or observed transactions cannot receive an absence disposition', () => {
  for (const changed of [{ blockhash_valid: true }, { blockhash_valid: null }, { current_height: 100 },
    { status: {} }, { transaction: {} }, { context: '02'.repeat(32) }]) {
    expect(() => assertAbsenceWitness({ ...witness(), ...changed })).toThrow();
  }
});

test('any changed balance or cumulative payout prevents terminal absence', () => {
  for (const changed of [{ balances: { ...witness().balances, processed: 9_000_000 } },
    { balances: { ...witness().balances, confirmed: 9_000_000 } }, { recipient_l1_balance: 1 },
    { receipt_withdrawn: '1000000' }, { receipt_total: '20000000' }]) {
    expect(() => assertAbsenceWitness({ ...witness(), ...changed })).toThrow();
  }
});
