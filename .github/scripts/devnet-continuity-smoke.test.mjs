// Sonic: Guard the funded adapter before reading keys or contacting a chain.
import { test, expect } from 'bun:test';
import { assertDevelopmentNetwork, runSmoke, sameMetadata, waitForBoundSession } from './devnet-continuity-smoke.mjs';

const DEVNET = 'EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG';
const MAINNET = '5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d';
const l1 = 'http://127.0.0.1:8899';
const er = 'http://127.0.0.1:8910';

test('canonical persistence does not change binding identity through object key ordering', () => {
  const expected = { version: 2, genesis: DEVNET, validator: 'configured-validator' };
  const persisted = { genesis: DEVNET, validator: 'configured-validator', version: 2 };
  expect(sameMetadata(persisted, expected)).toBe(true);
  expect(sameMetadata(persisted, { ...expected, validator: 'different' })).toBe(false);
});

test('mainnet and remote control roots are rejected', () => {
  expect(() => assertDevelopmentNetwork(MAINNET, l1, er, true, { lo: [] })).toThrow('Mainnet');
  for (const endpoint of ['http://example.com', 'http://user:secret@localhost', l1 + '/path', l1 + '?token=value']) {
    expect(() => assertDevelopmentNetwork(DEVNET, endpoint, er, false)).toThrow('loopback root');
  }
  assertDevelopmentNetwork(DEVNET, l1, er, false);
});

test('non-devnet qualification requires real sandbox loopback isolation', () => {
  expect(() => assertDevelopmentNetwork('local', l1, er, false, { lo: [] }, false)).toThrow('sandbox');
  expect(() => assertDevelopmentNetwork('local', l1, er, true, { lo: [], eth0: [] }, false)).toThrow('private loopback');
  assertDevelopmentNetwork('local', l1, er, true, { lo: [] }, false);
});

test('explicit host qualification stays loopback-only and never permits mainnet', () => {
  expect(() => assertDevelopmentNetwork('local', l1, er, false, { lo: [], eth0: [] }, true)).not.toThrow();
  expect(() => assertDevelopmentNetwork(MAINNET, l1, er, false, { lo: [] }, true)).toThrow('Mainnet');
  expect(() => assertDevelopmentNetwork('local', 'https://example.com', er, false, { lo: [] }, true)).toThrow('loopback');
});

test('recovery waits for the persisted L1 receipt floor despite a matching session context', async () => {
  let reads = 0, executions = 0;
  const readers = {
    snapshot: async () => {},
    syncStatus: async () => ({ isSyncing: false, latestSyncedSlot: reads >= 2 ? 112 : 101, latestL1Slot: reads >= 2 ? 112 : 101, checkpointError: null }),
    executionSession: async () => { executions++; },
    minimumL1Slot: () => 108,
    l1ConfirmedSlot: async () => ++reads === 1 ? 101 : 112,
    l1FinalizedSlot: async () => 112,
  };
  await waitForBoundSession(readers, 200, 1);
  expect(reads).toBeGreaterThanOrEqual(2);
  expect(executions).toBe(1);
});

test('unknown phases are rejected before any private configuration lookup', async () => {
  await expect(runSmoke('/not-a-control-file', '../keys')).rejects.toThrow('Unknown continuity');
});
