// Sonic: Read-only qualification; this module never loads a signer or submits a transaction.
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { readPrivateJson } from './continuity-control-journal.mjs';
import { assertDevelopmentNetwork } from './devnet-continuity-smoke.mjs';

const DEVNET = 'EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG';
const SDK_REVISION = 'ea732d51f71f62e6fe46c77eaf781b9474b00508';
function requireValue(condition, message) { if (!condition) throw new Error(message); }
function key(value) { return value.toBase58(); }

export async function snapshotIdentity(config, readers, mode) {
  requireValue(['online', 'l1'].includes(mode), 'Unknown native snapshot mode');
  requireValue(config.genesis === DEVNET && config.sdk_revision === SDK_REVISION && /^[0-9a-f]{64}$/.test(config.profile) && /[1-9a-f]/.test(config.profile), 'Invalid native upgrade descriptor');
  requireValue(await readers.genesis() === DEVNET, 'Native upgrade is devnet-only');
  const snapshot = await readers.snapshot();
  const session = key(snapshot.address);
  requireValue(session === key(await readers.sessionAddress()), 'Native session PDA differs');
  const state = snapshot.state;
  requireValue(state.nonce > 0n && key(state.validator) === config.validator, 'Native session validator or nonce differs');
  const context = Buffer.from(snapshot.context).toString('hex');
  requireValue(context === readers.context(snapshot), 'Native session context differs');
  const identity = {
    version: 2, genesis: DEVNET, portal: config.portal, token_bridge: config.token_bridge,
    validator: config.validator, session, context, profile: config.profile,
    authority: key(state.authority), nonce: state.nonce.toString(), grid_id: state.gridId.toString(),
    created_at: state.createdAt.toString(), ttl_slots: state.ttlSlots.toString(), fee_cap: state.feeCap.toString(),
    settlement_interval_slots: state.settlementIntervalSlots.toString(),
  };
  let er_genesis = null;
  if (mode === 'online') {
    requireValue(await readers.validator() === config.validator, 'Running validator identity differs');
    const status = await readers.syncStatus();
    // The pinned SDK normalizes RPC slots to bigint, even when the wire uses numbers.
    requireValue(status.checkpointError == null && status.isSyncing === false &&
      [status.latestSyncedSlot, status.latestL1Slot].every(value => typeof value === 'bigint' && value >= 0n && value <= 0xffff_ffff_ffff_ffffn) &&
      status.latestSyncedSlot >= status.latestL1Slot, 'Native checkpoint quarantine or synchronization blocks qualification');
    const execution = await readers.executionSession();
    requireValue(key(execution.address) === session && Buffer.from(execution.context).toString('hex') === context,
      'Execution session changed during native snapshot');
    er_genesis = await readers.erGenesis();
  }
  return { identity, er_genesis };
}

export async function readSnapshot(configPath, mode) {
  const config = readPrivateJson(configPath);
  assertDevelopmentNetwork(config.genesis, config.l1_rpc, config.er_rpc);
  const upstream = new URL(config.devnet_rpc);
  requireValue(upstream.protocol === 'https:' && !upstream.username && !upstream.password && !upstream.search && !upstream.hash,
    'Native upstream requires unauthenticated HTTPS');
  const dependencies = createRequire(path.join(config.sdk_checkout, 'package.json'));
  const { NorthStarSDK, sessionCommitment } = dependencies('./dist/index.js');
  const { PublicKey } = dependencies('@solana/web3.js');
  const bs58 = dependencies('bs58').default;
  const portal = new PublicKey(config.portal), validator = new PublicKey(config.validator);
  const sdk = new NorthStarSDK({
    portalProgramId: portal, tokenBridgeProgramId: new PublicKey(config.token_bridge), l1GenesisHash: DEVNET,
    checkpointProfile: Uint8Array.from(Buffer.from(config.profile, 'hex')),
    customEndpoints: { solana: mode === 'l1' ? config.devnet_rpc : config.l1_rpc, ephemeralRollup: config.er_rpc },
  });
  const l1 = sdk.getRpc(), er = sdk.getEphemeralRpc();
  return snapshotIdentity(config, {
    genesis: () => l1.getGenesisHash(), snapshot: () => sdk.getSessionSnapshot(),
    sessionAddress: () => sdk.portal.deriveSessionPDA(),
    context: snapshot => Buffer.from(sessionCommitment({ l1Genesis: bs58.decode(DEVNET), portal,
      session: snapshot.address, nonce: snapshot.state.nonce, gridId: snapshot.state.gridId,
      validator, profile: Uint8Array.from(Buffer.from(config.profile, 'hex')) })).toString('hex'),
    validator: async () => {
      const response = await l1._rpcRequest('getIdentity', []);
      requireValue(!response.error, 'Validator identity RPC failed');
      return response.result.identity;
    },
    syncStatus: () => sdk.getEphemeralRollupSyncStatus(), executionSession: () => sdk.getExecutionSession(),
    erGenesis: () => er.getGenesisHash(),
  }, mode);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [, , config, mode] = process.argv;
  console.log(JSON.stringify(await readSnapshot(config, mode)));
}
