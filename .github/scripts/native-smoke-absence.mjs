import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { ControlJournal, atomicPrivateJson, readPrivateJson } from './continuity-control-journal.mjs';

const DEVNET = 'EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG';
const REVISION = 'b545c054c35861b6ed01e3e4baf9939cffedec47';
const WIRE_HASH = '031e47c3e19b1fc1f9ff62958d6e6a0f4cb300e9fb3612293a780f91e3f0e46f';
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
function requireValue(condition, message) { if (!condition) throw new Error(message); }

export function assertAbsenceWitness(value) {
  requireValue(value.blockhash_valid === false && value.status === null && value.transaction === null,
    'Original transaction is not terminally absent');
  requireValue(Number.isSafeInteger(value.current_height) && Number.isSafeInteger(value.expiry_height) &&
    value.current_height > value.expiry_height && value.expiry_height >= 0, 'Original transaction lifetime has not expired');
  requireValue(['processed', 'confirmed', 'finalized'].every(level => value.balances[level] === 10_000_000) &&
    value.recipient_l1_balance === 0 && value.receipt_total === '10000000' && value.receipt_withdrawn === '0',
    'Original withdrawal state is not preserved');
  requireValue(value.context === value.expected_context && /^[0-9a-f]{64}$/.test(value.context), 'Session context differs');
}

export async function recordAbsence(configPath, outputPath) {
  const config = readPrivateJson(configPath);
  requireValue(config.source_revision === REVISION && config.genesis === DEVNET &&
    config.portal === 'AqMUacr8J4f7NDB5pTedi24Qhv3tuwCppRv1p6E3wjsr' &&
    config.l1_rpc === 'http://127.0.0.1:8899' && config.er_rpc === 'http://127.0.0.1:8910', 'Reviewed native identity differs');
  const dependencies = createRequire(path.join(config.sdk_checkout, 'package.json'));
  const { NorthStarSDK } = dependencies('./dist/index.js');
  const { PublicKey, VersionedTransaction } = dependencies('@solana/web3.js');
  const bs58 = dependencies('bs58').default;
  const sdk = new NorthStarSDK({ portalProgramId: new PublicKey(config.portal), tokenBridgeProgramId: new PublicKey(config.token_bridge),
    l1GenesisHash: config.genesis, checkpointProfile: Uint8Array.from(Buffer.from(config.profile, 'hex')),
    customEndpoints: { solana: config.l1_rpc, ephemeralRollup: config.er_rpc } });
  const l1 = sdk.getRpc(), er = sdk.getEphemeralRpc();
  requireValue(await l1.getGenesisHash() === DEVNET && await er.getGenesisHash() === config.er_genesis, 'RPC identity differs');
  const identity = await l1._rpcRequest('getIdentity', []);
  requireValue(identity.result?.identity === config.validator && !identity.error, 'Validator identity differs');
  const root = config.smoke_directory;
  const controls = path.join(root, 'controls');
  const stored = readPrivateJson(path.join(controls, 'binding.json'));
  const decode = wire => {
    const transaction = VersionedTransaction.deserialize(wire);
    return { signature: bs58.encode(transaction.signatures[0]), blockhash: transaction.message.recentBlockhash };
  };
  const journal = new ControlJournal(controls, stored.identity, decode, async () => { throw new Error('Absence inspection cannot submit'); });
  requireValue(journal.binding === stored.binding, 'Original journal binding differs');
  journal.attach('l1', l1);
  journal.attach('er', er);
  const directory = path.join(controls, 'sol-withdraw');
  const headerPath = path.join(directory, 'operation.json');
  const header = readPrivateJson(headerPath);
  requireValue(header.version === 2 && header.label === 'sol-withdraw' && header.layer === 'er' &&
    header.binding === journal.binding && header.request.amount === '1000000', 'Original semantic operation differs');
  const operation = { label: 'sol-withdraw', layer: 'er', directory,
    headerHash: sha(fs.readFileSync(headerPath, 'utf8').trim()) };
  const { intent } = journal.readIntent(operation);
  requireValue(intent.wire_sha256 === WIRE_HASH && !fs.existsSync(path.join(directory, 'receipt.json')) &&
    !fs.existsSync(path.join(directory, 'observed.json')) && !fs.existsSync(path.join(directory, 'fatal.json')), 'Original retained outcome differs');
  const index = readPrivateJson(path.join(root, 'keys/index.json'));
  const prepared = readPrivateJson(path.join(root, 'prepare-sol.json'));
  requireValue(prepared.payer === index['sol-payer'] && header.request.recipient === index['sol-recipient'], 'Original test keys differ');
  const current = await sdk.getSessionSnapshot();
  const context = Buffer.from(current.context).toString('hex');
  requireValue(current.state.nonce === 1n && current.state.validator.toBase58() === config.validator &&
    current.state.authority.toBase58() === config.funder && context === stored.identity.context && current.state.lastSettledErSlot > 0n,
    'Native SessionV2 binding or settlement differs');
  await sdk.getExecutionSession();
  const status = await sdk.getEphemeralRollupSyncStatus();
  requireValue(!status.isSyncing && status.checkpointError == null && status.latestSyncedSlot >= status.latestL1Slot, 'Native runtime is not ready');
  const floor = journal.receiptFloor('l1');
  requireValue(await l1.getSlot('confirmed') >= floor && await l1.getSlot('finalized') >= floor, 'L1 receipt floor not reached');
  const receiptAddress = await sdk.portal.deriveDepositReceiptPDA(current.context, new PublicKey(prepared.payer));
  const account = await l1.getAccountInfo(receiptAddress, 'finalized');
  requireValue(account?.owner.equals(new PublicKey(config.portal)), 'Original deposit receipt owner differs');
  const receipt = sdk.portal.parseDepositReceipt(account.data);
  const balances = {};
  for (const level of ['processed', 'confirmed', 'finalized']) balances[level] = await er.getBalance(new PublicKey(prepared.payer), level);
  const statuses = await er.getSignatureStatuses([intent.signature], { searchTransactionHistory: true });
  requireValue(Array.isArray(statuses.value) && statuses.value.length === 1, 'Malformed original signature status');
  const valid = await er.isBlockhashValid(intent.blockhash, { commitment: 'confirmed' });
  const witness = { blockhash_valid: valid.value, status: statuses.value[0],
    transaction: await er.getTransaction(intent.signature, { commitment: 'confirmed', maxSupportedTransactionVersion: 0 }),
    current_height: await er.getBlockHeight('confirmed'), expiry_height: intent.last_valid_block_height, balances,
    recipient_l1_balance: await l1.getBalance(new PublicKey(index['sol-recipient']), 'finalized'),
    receipt_total: receipt.totalDeposited.toString(), receipt_withdrawn: receipt.withdrawn.toString(),
    context, expected_context: stored.identity.context };
  assertAbsenceWitness(witness);
  const intentFile = path.join(directory, 'intent.json');
  const bound = { version: 1, disposition: 'not_included', binding: journal.binding, operation_sha256: intent.operation_sha256,
    signature: intent.signature, wire_sha256: intent.wire_sha256, intent_file_sha256: sha(fs.readFileSync(intentFile)),
    source_revision: REVISION, context, genesis: DEVNET };
  const filename = path.join(directory, 'absence.json');
  if (fs.existsSync(filename)) {
    const saved = readPrivateJson(filename);
    requireValue(Object.entries(bound).every(([key, value]) => saved[key] === value), 'Established absence disposition differs');
  } else atomicPrivateJson(filename, { ...bound, witness });
  const result = { ...bound, original_payer: prepared.payer, original_recipient: index['sol-recipient'],
    disposition_sha256: sha(fs.readFileSync(filename)) };
  if (outputPath) atomicPrivateJson(outputPath, result);
  return result;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  recordAbsence(process.argv[2], process.argv[3]).then(value => console.log(JSON.stringify(value))).catch(error => {
    console.error(error.message); process.exitCode = 1;
  });
}
