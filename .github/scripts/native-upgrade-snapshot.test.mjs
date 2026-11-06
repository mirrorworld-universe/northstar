import assert from 'node:assert/strict';
import test from 'node:test';
import { snapshotIdentity } from './native-upgrade-snapshot.mjs';

const DEVNET = 'EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG';
const key = value => ({ toBase58: () => value });
function fixture() {
  const config = { genesis: DEVNET, sdk_revision: '24030ce9fc3b80aa21738db2bb9f5a5fbea030d8',
    profile: '01'.repeat(32), portal: 'portal', token_bridge: 'bridge', validator: 'validator' };
  const state = { nonce: 2n, gridId: 3n, authority: key('authority'), validator: key('validator'),
    createdAt: 1n, ttlSlots: 1000n, feeCap: 10n, settlementIntervalSlots: 75n };
  const snapshot = { address: key('session'), state, context: Buffer.alloc(32, 0xab) };
  const calls = [];
  function read(name, value) { return async () => { calls.push(name); return value; }; }
  const readers = {
    genesis: read('genesis', DEVNET), snapshot: read('snapshot', snapshot),
    sessionAddress: read('session-address', key('session')), context: () => snapshot.context.toString('hex'),
    validator: read('validator', 'validator'), syncStatus: read('sync', { isSyncing: false, latestSyncedSlot: 100, latestL1Slot: 100 }),
    executionSession: read('execution', snapshot), erGenesis: read('er-genesis', 'er-genesis'),
  };
  return { config, snapshot, readers, calls };
}

test('online snapshot binds the existing nonce, profile, authority and execution context', async () => {
  const { config, readers, calls } = fixture();
  const result = await snapshotIdentity(config, readers, 'online');
  assert.equal(result.identity.nonce, '2');
  assert.equal(result.identity.authority, 'authority');
  assert.equal(result.identity.profile, config.profile);
  assert.equal(result.identity.created_at, '1');
  assert.equal(result.er_genesis, 'er-genesis');
  assert.deepEqual(calls, ['genesis', 'snapshot', 'session-address', 'validator', 'sync', 'execution', 'er-genesis']);
});

test('L1-only fence works while the validator is stopped without contacting ER', async () => {
  const { config, readers, calls } = fixture();
  const result = await snapshotIdentity(config, readers, 'l1');
  assert.equal(result.identity.context, 'ab'.repeat(32));
  assert.equal(result.er_genesis, null);
  assert.deepEqual(calls, ['genesis', 'snapshot', 'session-address']);
});

test('checkpoint quarantine never qualifies', async () => {
  const { config, readers, calls } = fixture();
  readers.syncStatus = async () => ({ checkpointError: 'checkpoint_lineage_mismatch', isSyncing: true });
  await assert.rejects(snapshotIdentity(config, readers, 'online'), /checkpoint quarantine/);
  assert(!calls.includes('execution'));
});

test('syncing ER never qualifies', async () => {
  const { config, readers } = fixture();
  readers.syncStatus = async () => ({ isSyncing: true, latestSyncedSlot: 99, latestL1Slot: 100 });
  await assert.rejects(snapshotIdentity(config, readers, 'online'), /synchronization/);
});

test('a different execution context is rejected', async () => {
  const { config, readers, snapshot } = fixture();
  readers.executionSession = async () => ({ ...snapshot, context: Buffer.alloc(32, 0xcd) });
  await assert.rejects(snapshotIdentity(config, readers, 'online'), /Execution session changed/);
});

test('network, validator, PDA and context bindings are required', async () => {
  for (const change of [
    f => { f.readers.genesis = async () => 'other-genesis'; },
    f => { f.readers.validator = async () => 'other-validator'; },
    f => { f.readers.sessionAddress = async () => key('other-session'); },
    f => { f.snapshot.state.validator = key('other-validator'); },
    f => { f.snapshot.state.nonce = 0n; },
    f => { f.readers.context = () => 'cd'.repeat(32); },
    f => { f.config.sdk_revision = '0'.repeat(40); },
    f => { f.config.profile = '0'.repeat(64); },
    f => { f.readers.syncStatus = async () => ({ isSyncing: false, latestSyncedSlot: -1, latestL1Slot: -1 }); },
    f => { f.readers.syncStatus = async () => ({ isSyncing: false, latestSyncedSlot: '100', latestL1Slot: 100 }); },
  ]) {
    const f = fixture();
    change(f);
    await assert.rejects(snapshotIdentity(f.config, f.readers, 'online'));
  }
});

test('invalid modes do not run any reader', async () => {
  const { config, readers, calls } = fixture();
  await assert.rejects(snapshotIdentity(config, readers, 'bootstrap'), /Unknown native snapshot mode/);
  assert.deepEqual(calls, []);
});
