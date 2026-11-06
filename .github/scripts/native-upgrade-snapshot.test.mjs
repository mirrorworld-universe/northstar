import assert from 'node:assert/strict';
import test from 'node:test';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import path from 'node:path';
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
    validator: read('validator', 'validator'), syncStatus: read('sync', { isSyncing: false, latestSyncedSlot: 100n, latestL1Slot: 100n }),
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

test('pinned SDK bigint sync cursors qualify without downcasting', async () => {
  const { config, readers } = fixture();
  const slot = (1n << 53n) + 1n;
  readers.syncStatus = async () => ({ isSyncing: false, latestSyncedSlot: slot, latestL1Slot: slot });
  const result = await snapshotIdentity(config, readers, 'online');
  assert.equal(result.identity.context, 'ab'.repeat(32));
  assert.equal(result.er_genesis, 'er-genesis');
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
  readers.syncStatus = async () => ({ isSyncing: true, latestSyncedSlot: 99n, latestL1Slot: 100n });
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
    f => { f.readers.syncStatus = async () => ({ isSyncing: false, latestSyncedSlot: -1n, latestL1Slot: -1n }); },
    f => { f.readers.syncStatus = async () => ({ isSyncing: false, latestSyncedSlot: '100', latestL1Slot: 100n }); },
    f => { f.readers.syncStatus = async () => ({ isSyncing: false, latestSyncedSlot: 100, latestL1Slot: 100 }); },
    f => { f.readers.syncStatus = async () => ({ isSyncing: false, latestSyncedSlot: 1n << 64n, latestL1Slot: 1n << 64n }); },
  ]) {
    const f = fixture();
    change(f);
    await assert.rejects(snapshotIdentity(f.config, f.readers, 'online'));
  }
});

const sdkCheckout = process.env.NORTHSTAR_TEST_SDK_CHECKOUT;
test('pinned public SDK wire normalization reaches native qualification', { skip: !sdkCheckout }, async t => {
  const dependencies = createRequire(path.join(sdkCheckout, 'package.json'));
  const { NorthStarSDK, PublicKey } = dependencies('./dist/index.js');
  let wireSlot = 100;
  const methods = [];
  const server = createServer(async (request, response) => {
    let body = '';
    for await (const chunk of request) body += chunk;
    const message = JSON.parse(body);
    methods.push(message.method);
    response.setHeader('Content-Type', 'application/json');
    response.end(JSON.stringify({ jsonrpc: '2.0', id: message.id,
      result: { isSyncing: false, latestSyncedSlot: wireSlot, latestL1Slot: wireSlot } }));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
  const savedProxy = [process.env.NO_PROXY, process.env.no_proxy];
  process.env.NO_PROXY = process.env.no_proxy = '127.0.0.1,localhost';
  t.after(() => {
    for (const [name, value] of [['NO_PROXY', savedProxy[0]], ['no_proxy', savedProxy[1]]]) {
      if (value === undefined) delete process.env[name]; else process.env[name] = value;
    }
  });
  const rpc = `http://127.0.0.1:${server.address().port}`;
  const sdk = new NorthStarSDK({
    portalProgramId: new PublicKey(Uint8Array.from({ length: 32 }, () => 1)),
    tokenBridgeProgramId: new PublicKey(Uint8Array.from({ length: 32 }, () => 2)),
    l1GenesisHash: DEVNET, checkpointProfile: Uint8Array.from({ length: 32 }, () => 1),
    customEndpoints: { solana: rpc, ephemeralRollup: rpc },
  });
  for (const wire of [100, '9007199254740993']) {
    wireSlot = wire;
    const status = await sdk.getEphemeralRollupSyncStatus();
    assert.equal(status.latestSyncedSlot, BigInt(wire));
    assert.equal(status.latestL1Slot, BigInt(wire));
    const { config, readers } = fixture();
    readers.syncStatus = () => sdk.getEphemeralRollupSyncStatus();
    assert.equal((await snapshotIdentity(config, readers, 'online')).identity.context, 'ab'.repeat(32));
  }
  assert.deepEqual(methods, Array(4).fill('northstarSysGetSyncStatus'));
});

test('invalid modes do not run any reader', async () => {
  const { config, readers, calls } = fixture();
  await assert.rejects(snapshotIdentity(config, readers, 'bootstrap'), /Unknown native snapshot mode/);
  assert.deepEqual(calls, []);
});
