// Sonic: Native v2 bootstrap and restart/payout qualification using the pinned public SDK.
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { isDeepStrictEqual } from 'node:util';
import { ControlJournal, atomicPrivateJson, readPrivateJson } from './continuity-control-journal.mjs';

const DEVNET = 'EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG';
const MAINNET = '5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d';
const SDK_REVISION = 'beb895f5178bdb21fbfe13786981b19241d3d8c4';
export const sameMetadata = isDeepStrictEqual;
function requireValue(condition, message) { if (!condition) throw new Error(message); }
function hex(value) { return Buffer.from(value).toString('hex'); }
function immutable(filename, value) {
  if (fs.existsSync(filename)) requireValue(sameMetadata(readPrivateJson(filename), value), 'Established smoke state changed');
  else atomicPrivateJson(filename, value);
}

export function assertDevelopmentNetwork(genesis, l1, er, sandbox = process.env.NORTHSTAR_SANDBOX === '1', interfaces = os.networkInterfaces(), hostOptIn = process.env.NORTHSTAR_ALLOW_LOCAL_HOST_E2E === '1') {
  requireValue(genesis !== MAINNET, 'Mainnet is never eligible for deployment qualification');
  for (const endpoint of [l1, er]) {
    const url = new URL(endpoint);
    requireValue(['http:', 'https:'].includes(url.protocol) && !url.username && !url.password &&
      ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname) && url.pathname === '/' && !url.search && !url.hash,
      'Control RPC must be a colocated loopback root');
    if (genesis !== DEVNET) requireValue(sandbox || hostOptIn, 'Non-devnet tests require local sandbox RPC or explicit host qualification');
  }
  if (genesis !== DEVNET && !hostOptIn) requireValue(Object.keys(interfaces).every(name => name === 'lo'), 'Local qualification requires private loopback networking');
}

export async function waitForBoundSession(readers, timeoutMs = 180_000, pollMs = 250) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    const floor = readers.minimumL1Slot();
    const slots = await Promise.all([readers.l1ConfirmedSlot(), readers.l1FinalizedSlot()]);
    requireValue([floor, ...slots].every(value => Number.isSafeInteger(value) && value >= 0), 'Invalid L1 recovery slot');
    if (slots.every(value => value >= floor)) {
      await readers.snapshot();
      const status = await readers.syncStatus();
      requireValue(status.checkpointError == null, 'Native checkpoint quarantine blocks qualification');
      if (!status.isSyncing && status.latestSyncedSlot >= status.latestL1Slot && status.latestL1Slot >= floor) {
        try { await readers.executionSession(); return; } catch { /* The ER view may still be importing the new frame. */ }
      }
    }
    await new Promise(resolve => setTimeout(resolve, pollMs));
  }
  throw new Error('Bound ER session readiness timed out');
}

export async function runSmoke(configPath, phase) {
  requireValue(['bootstrap', 'prepare-sol', 'resume-sol', 'prepare-spl', 'resume-spl'].includes(phase), 'Unknown continuity qualification phase');
  const config = readPrivateJson(configPath);
  requireValue(config.version === 2 && config.sdk_revision === SDK_REVISION && /^[0-9a-f]{64}$/.test(config.profile) && /[1-9a-f]/.test(config.profile), 'Invalid continuity deployment descriptor');
  assertDevelopmentNetwork(config.genesis, config.l1_rpc, config.er_rpc);
  const dependencies = createRequire(path.join(config.sdk_checkout, 'package.json'));
  const sdkModule = dependencies('./dist/index.js');
  const { NorthStarSDK, sessionCommitment, signVersionedTransaction } = sdkModule;
  const { Keypair, PublicKey, SystemProgram, TransactionMessage, VersionedTransaction } = dependencies('@solana/web3.js');
  const token = dependencies('@solana/spl-token');
  const bs58 = dependencies('bs58').default;
  const portal = new PublicKey(config.portal);
  const bridge = new PublicKey(config.token_bridge);
  const validator = new PublicKey(config.validator);
  const sdk = new NorthStarSDK({
    portalProgramId: portal, tokenBridgeProgramId: bridge, l1GenesisHash: config.genesis,
    checkpointProfile: Uint8Array.from(Buffer.from(config.profile, 'hex')),
    customEndpoints: { solana: config.l1_rpc, ephemeralRollup: config.er_rpc },
  });
  const l1 = sdk.getRpc(), er = sdk.getEphemeralRpc();
  requireValue(await l1.getGenesisHash() === config.genesis && await er.getGenesisHash() === config.er_genesis, 'RPC genesis changed');
  async function checkValidator() {
    const response = await l1._rpcRequest('getIdentity', []);
    requireValue(response?.result?.identity === config.validator && !response.error, 'Validator identity changed');
  }
  await checkValidator();
  function keyFromFile(filename) {
    const bytes = readPrivateJson(filename);
    requireValue(Array.isArray(bytes) && bytes.length === 64 && bytes.every(value => Number.isInteger(value) && value >= 0 && value <= 255), 'Invalid private key file');
    return Keypair.fromSecretKey(Uint8Array.from(bytes));
  }
  const funder = keyFromFile(config.funder_keypair);
  const session = await sdk.portal.deriveSessionPDA();
  const expectedContext = hex(sessionCommitment({
    l1Genesis: bs58.decode(config.genesis), portal, session, nonce: 1n, gridId: BigInt(config.grid_id),
    validator, profile: Uint8Array.from(Buffer.from(config.profile, 'hex')),
  }));
  const binding = {
    version: 2, genesis: config.genesis, er_genesis: config.er_genesis, l1_rpc: config.l1_rpc, er_rpc: config.er_rpc,
    source_revision: config.source_revision, sdk_revision: SDK_REVISION, portal: config.portal,
    token_bridge: config.token_bridge, validator: config.validator, authority: funder.publicKey.toBase58(),
    context: expectedContext, grid_id: config.grid_id, ttl_slots: config.ttl_slots,
    fee_cap: config.fee_cap, settlement_interval_slots: config.settlement_interval_slots,
  };
  const root = path.resolve(config.smoke_directory);
  if (!fs.existsSync(root)) {
    const temporary = root + '.' + crypto.randomUUID() + '.tmp';
    fs.mkdirSync(temporary, { mode: 0o700 });
    fs.mkdirSync(path.join(temporary, 'keys'), { mode: 0o700 });
    atomicPrivateJson(path.join(temporary, 'keys/index.json'), {});
    atomicPrivateJson(path.join(temporary, 'binding.json'), binding);
    fs.renameSync(temporary, root);
    const parent = fs.openSync(path.dirname(root), 'r');
    try { fs.fsyncSync(parent); } finally { fs.closeSync(parent); }
  }
  immutable(path.join(root, 'binding.json'), binding);
  const keyIndexPath = path.join(root, 'keys/index.json');
  const keyIndex = readPrivateJson(keyIndexPath);
  function key(name) {
    const filename = path.join(root, 'keys', name + '.json');
    requireValue(!keyIndex[name] || fs.existsSync(filename), 'Established smoke key is missing');
    let pair;
    if (fs.existsSync(filename)) pair = keyFromFile(filename);
    else {
      pair = Keypair.generate();
      atomicPrivateJson(filename, Array.from(pair.secretKey));
    }
    const address = pair.publicKey.toBase58();
    requireValue(!keyIndex[name] || keyIndex[name] === address, 'Established smoke key changed');
    if (!keyIndex[name]) { keyIndex[name] = address; atomicPrivateJson(keyIndexPath, keyIndex); }
    return pair;
  }
  async function snapshot() {
    const current = await sdk.getSessionSnapshot();
    requireValue(hex(current.context) === expectedContext && current.state.nonce === 1n &&
      current.state.authority.equals(funder.publicKey) && current.state.validator.equals(validator) &&
      current.state.gridId === BigInt(config.grid_id) && current.state.ttlSlots === BigInt(config.ttl_slots) &&
      current.state.feeCap === BigInt(config.fee_cap) &&
      current.state.settlementIntervalSlots === BigInt(config.settlement_interval_slots), 'Native SessionV2 identity differs');
    return current;
  }
  async function ready(timeoutMs = 180_000) {
    return waitForBoundSession({ snapshot,
      syncStatus: () => sdk.getEphemeralRollupSyncStatus(), executionSession: () => sdk.getExecutionSession(),
      l1ConfirmedSlot: () => l1.getSlot('confirmed'), l1FinalizedSlot: () => l1.getSlot('finalized'),
      minimumL1Slot: () => journal.receiptFloor('l1'),
    }, timeoutMs);
  }
  const journal = new ControlJournal(path.join(root, 'controls'), binding,
    wire => {
      const transaction = VersionedTransaction.deserialize(wire);
      return { signature: bs58.encode(transaction.signatures[0]), blockhash: transaction.message.recentBlockhash };
    },
    async (label, _layer, options) => {
      requireValue(await l1.getGenesisHash() === config.genesis && await er.getGenesisHash() === config.er_genesis, 'RPC genesis changed');
      await checkValidator();
      const account = await l1.getAccountInfo(session, 'confirmed');
      if (label === 'open-session' && account === null) return;
      if (label !== 'open-session' && options.submit) await ready(15_000);
      else await snapshot();
    });
  journal.attach('l1', l1);
  journal.attach('er', er);
  const transactionOptions = { skipPreflight: false, maxAttempts: 120, intervalMs: 500 };
  const wallet = async transaction => signVersionedTransaction(transaction, [funder]);
  async function poll(label, read, predicate, timeoutMs = 300_000) {
    const end = Date.now() + timeoutMs;
    while (Date.now() < end) {
      const status = await sdk.getEphemeralRollupSyncStatus();
      requireValue(status.checkpointError == null, 'Native checkpoint quarantine: ' + label);
      const value = await read();
      if (predicate(value)) return value;
      await new Promise(resolve => setTimeout(resolve, 500));
    }
    throw new Error('Continuity phase timed out: ' + label);
  }
  async function send(label, layer, parameters, build, signers = [funder], payer = funder.publicKey) {
    return journal.run(label, layer, parameters, async () => {
      const rpc = layer === 'l1' ? l1 : er;
      const latest = await rpc.getLatestBlockhash(layer === 'l1' ? 'confirmed' : 'processed');
      const transaction = new VersionedTransaction(new TransactionMessage({
        payerKey: payer, recentBlockhash: latest.blockhash, instructions: await build(),
      }).compileToV0Message());
      transaction.sign(signers);
      await rpc.sendRawTransaction(transaction.serialize(), { skipPreflight: false, maxRetries: 0 });
    });
  }
  async function deposit(label, recipient, amount) {
    return journal.run(label, 'l1', { recipient: recipient.toBase58(), lamports: amount.toString() },
      () => sdk.depositFee(funder.publicKey, amount, wallet, {}, transactionOptions, recipient));
  }
  async function tokenAmount(address, commitment = 'finalized') {
    return (await token.getAccount(l1, address, commitment, token.TOKEN_PROGRAM_ID)).amount;
  }
  async function erAmount(address) {
    const account = await er.getAccountInfo(address, 'confirmed');
    if (account === null) return null;
    requireValue(account?.owner.equals(bridge), 'ER token owner differs');
    return sdk.tokenBridge.parseErTokenAccount(account.data).amount;
  }
  const completion = path.join(root, phase + '.json');
  if (fs.existsSync(completion)) {
    await ready();
    const saved = readPrivateJson(completion);
    requireValue(saved.phase === phase && saved.context === expectedContext, 'Established qualification receipt differs');
    return saved;
  }
  let result;
  if (phase === 'bootstrap') {
    const existing = await l1.getAccountInfo(session, 'confirmed');
    requireValue(existing === null || fs.existsSync(path.join(root, 'controls/open-session/intent.json')), 'Unrecorded native session already exists');
    const opened = await journal.run('open-session', 'l1', {
      validator: config.validator, ttl_slots: config.ttl_slots, fee_cap: config.fee_cap,
      settlement_interval_slots: config.settlement_interval_slots,
    }, () => sdk.openSession(funder.publicKey, BigInt(config.grid_id), BigInt(config.ttl_slots), BigInt(config.fee_cap),
      wallet, {}, transactionOptions, { validator, settlementIntervalSlots: BigInt(config.settlement_interval_slots) }));
    await ready();
    result = { phase, context: expectedContext, nonce: '1', session: session.toBase58(), signature: opened.signature };
  } else {
    await ready();
    const solPayer = key('sol-payer'), solRecipient = key('sol-recipient');
    if (phase === 'prepare-sol') {
      await deposit('sol-deposit', solPayer.publicKey, 10_000_000n);
      await poll('SOL credit', () => er.getBalance(solPayer.publicKey, 'confirmed'), balance => balance === 10_000_000);
      result = { phase, context: expectedContext, payer: solPayer.publicKey.toBase58(), er_balance: 10_000_000,
        er_slot: await er.getSlot('confirmed'), last_settled_er_slot: (await snapshot()).state.lastSettledErSlot.toString() };
    } else if (phase === 'resume-sol') {
      if (!fs.existsSync(path.join(root, 'controls/sol-withdraw/intent.json'))) {
        requireValue(await er.getBalance(solPayer.publicKey, 'confirmed') === 10_000_000, 'SOL restart balance differs');
      }
      const withdrawal = await send('sol-withdraw', 'er', { amount: '1000000', recipient: solRecipient.publicKey.toBase58() },
        () => sdk.buildErSolWithdrawalInstructions({ erSource: solPayer.publicKey, l1Recipient: solRecipient.publicKey, lamports: 1_000_000n }),
        [solPayer], solPayer.publicKey);
      await poll('SOL finalized L1 payout', () => l1.getBalance(solRecipient.publicKey, 'finalized'), balance => balance === 1_000_000);
      const address = await sdk.portal.deriveDepositReceiptPDA((await snapshot()).context, solPayer.publicKey);
      await poll('SOL cumulative payout receipt', async () => {
        const account = await l1.getAccountInfo(address, 'finalized');
        requireValue(account?.owner.equals(portal), 'SOL receipt owner changed');
        return sdk.portal.parseDepositReceipt(account.data);
      }, value => value.withdrawn === 1_000_000n && value.totalDeposited === 10_000_000n);
      result = { phase, context: expectedContext, signature: withdrawal.signature, l1_payout_lamports: 1_000_000 };
    } else if (phase === 'prepare-spl' || phase === 'resume-spl') {
      const mint = key('spl-mint'), bob = key('spl-bob'), feePayer = key('spl-fee-payer');
      const sessionBridge = await sdk.portal.deriveSessionBridgePDA((await snapshot()).context, mint.publicKey);
      const vault = sdk.tokenBridge.deriveVaultPDA(sessionBridge);
      const aliceToken = token.getAssociatedTokenAddressSync(mint.publicKey, funder.publicKey);
      const bobToken = token.getAssociatedTokenAddressSync(mint.publicKey, bob.publicKey);
      const vaultToken = token.getAssociatedTokenAddressSync(mint.publicKey, vault, true);
      const aliceEr = sdk.tokenBridge.deriveErTokenAccountPDA(sessionBridge, funder.publicKey);
      const bobEr = sdk.tokenBridge.deriveErTokenAccountPDA(sessionBridge, bob.publicKey);
      if (phase === 'prepare-spl') {
        await deposit('spl-manager-credit', validator, 100_000_000n);
        await deposit('spl-payer-credit', feePayer.publicKey, 100_000_000n);
        await poll('manager SOL credit', () => er.getBalance(validator, 'confirmed'), balance => balance >= 50_000_000);
        await poll('SPL payer SOL credit', () => er.getBalance(feePayer.publicKey, 'confirmed'), balance => balance >= 50_000_000);
        await send('spl-create-mint', 'l1', { mint: mint.publicKey.toBase58(), bob: bob.publicKey.toBase58() }, async () => [
          SystemProgram.createAccount({ fromPubkey: funder.publicKey, newAccountPubkey: mint.publicKey,
            lamports: await l1.getMinimumBalanceForRentExemption(token.MINT_SIZE), space: token.MINT_SIZE, programId: token.TOKEN_PROGRAM_ID }),
          token.createInitializeMintInstruction(mint.publicKey, 6, funder.publicKey, null),
          token.createAssociatedTokenAccountInstruction(funder.publicKey, aliceToken, funder.publicKey, mint.publicKey),
          token.createAssociatedTokenAccountInstruction(funder.publicKey, bobToken, bob.publicKey, mint.publicKey),
          token.createAssociatedTokenAccountInstruction(funder.publicKey, vaultToken, vault, mint.publicKey),
          token.createMintToInstruction(mint.publicKey, aliceToken, funder.publicKey, 1_000_000_000n),
        ], [funder, mint]);
        await send('spl-register-vault', 'l1', { mint: mint.publicKey.toBase58(), owner: bob.publicKey.toBase58() }, async () => [
          await sdk.buildRegisterSessionBridgeInstruction({ payer: funder.publicKey, session, mint: mint.publicKey, vault, tokenProgram: token.TOKEN_PROGRAM_ID }),
          await sdk.buildInitializeTokenVaultInstruction({ payer: funder.publicKey, sessionBridge, vaultTokenAccount: vaultToken }),
          await sdk.buildInitializeErTokenAccountInstruction({ payer: funder.publicKey, sessionBridge, owner: funder.publicKey }),
          await sdk.buildInitializeErTokenAccountInstruction({ payer: funder.publicKey, sessionBridge, owner: bob.publicKey }),
        ]);
        await send('spl-delegate', 'l1', { mint: mint.publicKey.toBase58(), owner: bob.publicKey.toBase58() }, async () => {
          const current = await snapshot();
          return [funder, bob].map((owner, index) => sdk.buildDelegateErTokenAccountInstruction({
            payer: funder.publicKey, owner: owner.publicKey, erTokenAccount: index ? bobEr : aliceEr,
            sessionBridge, session, mint: mint.publicKey, gridId: BigInt(config.grid_id),
            sessionContext: current.context, sequence: current.nextSequence + BigInt(index),
          }));
        }, [funder, bob]);
        await poll('delegated SPL accounts', async () => [await erAmount(aliceEr), await erAmount(bobEr)], amounts => amounts.every(value => value === 0n));
        await send('spl-deposit', 'l1', { mint: mint.publicKey.toBase58(), amount: '600000000' }, async () => {
          const current = await snapshot();
          return [sdk.buildTokenBridgeDepositInstruction({ session, sessionContext: current.context, sequence: current.nextSequence,
            payer: funder.publicKey, owner: funder.publicKey, vault, erTokenAccount: aliceEr, sessionBridge,
            sourceTokenAccount: aliceToken, vaultTokenAccount: vaultToken, mint: mint.publicKey, tokenProgram: token.TOKEN_PROGRAM_ID,
            amount: 600_000_000n, decimals: 6 })];
        });
        await poll('SPL credit', () => erAmount(aliceEr), amount => amount === 600_000_000n);
        await send('spl-transfer', 'er', { mint: mint.publicKey.toBase58(), amount: '250000000' }, async () => [sdk.buildTokenBridgeTransferInstruction({
          authority: funder.publicKey, sourceErTokenAccount: aliceEr, destinationErTokenAccount: bobEr, amount: 250_000_000n,
        })], [feePayer, funder], feePayer.publicKey);
        await poll('ER SPL transfer', async () => [await erAmount(aliceEr), await erAmount(bobEr)], amounts => amounts[0] === 350_000_000n && amounts[1] === 250_000_000n);
        result = { phase, context: expectedContext, mint: mint.publicKey.toBase58(), alice_er: aliceEr.toBase58(), bob_er: bobEr.toBase58(),
          er_slot: await er.getSlot('confirmed'), last_settled_er_slot: (await snapshot()).state.lastSettledErSlot.toString() };
      } else {
        requireValue(await erAmount(aliceEr) === 350_000_000n, 'SPL restart source balance differs');
        if (!fs.existsSync(path.join(root, 'controls/spl-withdraw/intent.json'))) {
          requireValue(await erAmount(bobEr) === 250_000_000n, 'SPL restart destination balance differs');
        }
        const withdrawal = await send('spl-withdraw', 'er', { mint: mint.publicKey.toBase58(), amount: '200000000' }, async () => [sdk.buildTokenBridgeStartWithdrawalInstruction({
          sessionContext: (await snapshot()).context, mint: mint.publicKey, owner: bob.publicKey, erTokenAccount: bobEr,
          sessionBridge, destinationTokenAccount: bobToken, tokenProgram: token.TOKEN_PROGRAM_ID, amount: 200_000_000n, decimals: 6,
        })], [feePayer, bob], feePayer.publicKey);
        await poll('SPL finalized L1 payout', () => tokenAmount(bobToken), amount => amount === 200_000_000n);
        requireValue(await tokenAmount(vaultToken) === 400_000_000n && await erAmount(bobEr) === 50_000_000n, 'SPL payout custody or ER debit differs');
        requireValue((await l1.getAccountInfo(bobEr, 'finalized'))?.owner.equals(portal), 'L1 SPL owner changed');
        result = { phase, context: expectedContext, signature: withdrawal.signature, l1_payout_tokens: '200000000' };
      }
    } else throw new Error('Unknown continuity qualification phase');
  }
  immutable(completion, result);
  return result;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runSmoke(process.argv[2], process.argv[3]).then(value => console.log(JSON.stringify(value))).catch(error => {
    console.error(error instanceof Error ? error.message : 'Continuity qualification failed');
    process.exitCode = 1;
  });
}
