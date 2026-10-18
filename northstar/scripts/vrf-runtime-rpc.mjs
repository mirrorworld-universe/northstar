#!/usr/bin/env bun
// Read-only RPC/simulation client. It has no signing, funding, deployment, or sendTransaction path.
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';

const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
const hash = bytes => createHash('sha256').update(bytes).digest();
const concat = (...bytes) => Buffer.concat(bytes.map(x => Buffer.from(x)));
export function base58(bytes) {
  bytes = Buffer.from(bytes);
  let n = BigInt('0x' + (bytes.toString('hex') || '0'));
  let result = '';
  while (n) { result = ALPHABET[Number(n % 58n)] + result; n /= 58n; }
  for (const byte of bytes) { if (byte !== 0) break; result = '1' + result; }
  return result;
}
export function unbase58(text) {
  if (typeof text !== 'string' || !text.length) throw new Error('invalid base58');
  let n = 0n;
  for (const char of text) {
    const digit = ALPHABET.indexOf(char);
    if (digit < 0) throw new Error('invalid base58');
    n = n * 58n + BigInt(digit);
  }
  let hex = n ? n.toString(16) : '';
  if (hex.length % 2) hex = '0' + hex;
  return concat(Buffer.alloc(text.match(/^1*/)[0].length), Buffer.from(hex, 'hex'));
}
function key(text) {
  const bytes = unbase58(text);
  if (bytes.length !== 32) throw new Error('pubkey must decode to 32 bytes');
  return bytes;
}
const P = (1n << 255n) - 19n;
const mod = x => ((x % P) + P) % P;
function pow(x, n) {
  let value = 1n;
  for (; n; n >>= 1n, x = mod(x * x)) if (n & 1n) value = mod(value * x);
  return value;
}
function littleInteger(bytes) {
  return BigInt('0x' + Buffer.from(bytes).reverse().toString('hex'));
}
function onEd25519Curve(bytes) {
  const y = littleInteger(bytes) & ((1n << 255n) - 1n);
  const y2 = mod(y * y);
  const d = mod(-121665n * pow(121666n, P - 2n));
  const denominator = mod(d * y2 + 1n);
  if (!denominator) return false;
  const x2 = mod((y2 - 1n) * pow(denominator, P - 2n));
  return x2 === 0n || pow(x2, (P - 1n) / 2n) === 1n;
}
export function authority(program, queue) {
  for (let bump = 255; bump >= 0; bump--) {
    const bytes = hash(concat(Buffer.from('vrf-runtime-probe'), key(queue), [bump], key(program),
      Buffer.from('ProgramDerivedAddress')));
    if (!onEd25519Curve(bytes)) return { address: base58(bytes), bump };
  }
  throw new Error('no PDA bump');
}
export const CLOCK = 'SysvarC1ock11111111111111111111111111111111';
export const SLOT_HASHES = 'SysvarS1otHashes111111111111111111111111111';
export const FEATURES = {
  curve25519: '7rcw5UtqgDTBBv2EcynNfYckgdAaH1MAsCjKgXMkN7Ri',
  msm_length_compatibility: 'eca6zf6JJRjQsYYPkBHF3N32MTzur4n2WL4QiiacPCL',
  invalid_curve_compatibility: 'FuS3FPfJDKSNot99ECLXtp3rueq36hMNStJkPJwWodLh',
};
const FEATURE_OWNER = 'Feature111111111111111111111111111111111111';
export function u64(value) {
  const bytes = Buffer.alloc(8); bytes.writeBigUInt64LE(BigInt(value)); return bytes;
}
export function fixture(config = {}) {
  const defaults = { probeProgram: 81, consumerProgram: 82, queue: 83, consumerState: 84, payer: 85 };
  if (!config || typeof config !== 'object' || Array.isArray(config)) throw new Error('fixture config must be an object');
  if (Object.keys(config).some(name => !Object.hasOwn(defaults, name))) throw new Error('unknown fixture config field');
  config = Object.fromEntries(Object.entries(defaults).map(([name, byte]) =>
    [name, config[name] ?? base58(Buffer.alloc(32, byte))]));
  for (const value of Object.values(config)) key(value);
  if (new Set(Object.values(config)).size !== 5) throw new Error('fixture account aliases are not allowed');
  const pda = authority(config.probeProgram, config.queue);
  if (Object.values(config).includes(pda.address)) throw new Error('authority aliases a fixture account');
  return { config, authority: pda, nonce: '7',
    queueData: concat(key(config.consumerProgram), u64(7), [1]),
    stateData: concat(key(config.probeProgram), Buffer.alloc(24)) };
}
function short(value) {
  const bytes = [];
  do { let byte = value & 127; value >>>= 7; if (value) byte |= 128; bytes.push(byte); } while (value);
  return Buffer.from(bytes);
}
export function simulationTransaction(payer, instruction) {
  const entries = new Map([[payer, { key: payer, signer: true, writable: true }]]);
  for (const meta of [...instruction.accounts, { key: instruction.program, signer: false, writable: false }]) {
    const old = entries.get(meta.key);
    entries.set(meta.key, { key: meta.key, signer: Boolean(old?.signer || meta.signer),
      writable: Boolean(old?.writable || meta.writable) });
  }
  const rank = meta => meta.signer ? (meta.writable ? 0 : 1) : (meta.writable ? 2 : 3);
  const accounts = [...entries.values()].sort((a, b) => rank(a) - rank(b));
  const signers = accounts.filter(x => x.signer).length;
  if (signers !== 1) throw new Error('only the unsigned simulation payer may be a transaction signer');
  const index = address => accounts.findIndex(x => x.key === address);
  const message = concat([signers, 0, accounts.filter(x => !x.signer && !x.writable).length],
    short(accounts.length), ...accounts.map(x => key(x.key)), Buffer.alloc(32), [1],
    [index(instruction.program)], short(instruction.accounts.length), instruction.accounts.map(x => index(x.key)),
    short(instruction.data.length), instruction.data);
  const transaction = concat([1], Buffer.alloc(64), message);
  if (transaction.length > 1232) throw new Error('fixture exceeds the transaction packet limit');
  return transaction.toString('base64');
}
export function cases(f) {
  const c = f.config;
  const metas = () => [
    { key: c.queue, writable: true }, { key: c.consumerState, writable: true },
    { key: f.authority.address }, { key: c.consumerProgram }, { key: CLOCK },
  ];
  const instruction = (mode = 0, nonce = 7, slot = 0) => ({ program: c.probeProgram,
    accounts: metas(), data: concat([1, mode], u64(nonce), u64(slot)) });
  const error = value => ({ InstructionError: [0, value] });
  const result = [
    { name: 'curve_known_answers', instruction: { program: c.probeProgram, accounts: [], data: Buffer.from([0]) }, expected: null },
    { name: 'callback_success', instruction: instruction(), expected: null },
    { name: 'unsigned_cpi', instruction: instruction(1), expected: error('PrivilegeEscalation') },
    { name: 'callback_failure', instruction: instruction(2), expected: error({ Custom: 65261 }) },
    { name: 'wrong_nonce', instruction: instruction(0, 8), expected: error('InvalidInstructionData') },
    { name: 'future_slot', instruction: instruction(0, 7, (1n << 64n) - 1n), expected: error({ Custom: 256 }) },
  ];
  for (const [name, index] of [['readonly_queue', 0], ['readonly_state', 1]]) {
    const ix = instruction(); ix.accounts[index].writable = false;
    result.push({ name, instruction: ix, expected: error('InvalidAccountData') });
  }
  const wrongAuthority = instruction(); wrongAuthority.accounts[2].key = c.queue;
  result.push({ name: 'wrong_authority', instruction: wrongAuthority, expected: error('InvalidSeeds') });
  const wrongConsumer = instruction(); wrongConsumer.accounts[3].key = c.probeProgram;
  result.push({ name: 'wrong_consumer', instruction: wrongConsumer, expected: error('IncorrectProgramId') });
  result.push({ name: 'direct_callback', instruction: { program: c.consumerProgram, accounts: [...metas().slice(0, 3), { key: CLOCK }],
    data: concat([2, 0], u64(7)) }, expected: error('MissingRequiredSignature') });
  return result;
}
function accountBytes(account) {
  if (!account || !Array.isArray(account.data) || account.data.length !== 2 ||
      typeof account.data[0] !== 'string' || account.data[1] !== 'base64') throw new Error('base64 account required');
  const bytes = Buffer.from(account.data[0], 'base64');
  if (bytes.toString('base64') !== account.data[0]) throw new Error('non-canonical base64 account data');
  return bytes;
}
export function parseClock(account) {
  const b = accountBytes(account);
  if (account.owner !== 'Sysvar1111111111111111111111111111111111111' || b.length !== 40) throw new Error('unexpected Clock layout/owner');
  return { slot: b.readBigUInt64LE(0).toString(), epochStartTimestamp: b.readBigInt64LE(8).toString(),
    epoch: b.readBigUInt64LE(16).toString(), leaderScheduleEpoch: b.readBigUInt64LE(24).toString(),
    unixTimestamp: b.readBigInt64LE(32).toString() };
}
function parseSlotHashes(account) {
  const b = accountBytes(account);
  if (b.length < 8) throw new Error('unexpected SlotHashes layout');
  const count = b.readBigUInt64LE();
  if (account.owner !== 'Sysvar1111111111111111111111111111111111111' || count > 512n ||
      b.length < 8 + Number(count) * 40 || b.length > 20488) throw new Error('unexpected SlotHashes layout/owner');
  return { count: count.toString(), trailingBytes: b.length - 8 - Number(count) * 40,
    newestSlot: count ? b.readBigUInt64LE(8).toString() : null,
    oldestSlot: count ? b.readBigUInt64LE(8 + (Number(count) - 1) * 40).toString() : null,
    interpretation: 'Observation only; do not treat SlotHashes as an ER block or randomness source.' };
}
function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function rpcResponse(body, requestId) {
  const invalid = reason => ({ ok: false, protocolError: true, error: `Invalid JSON-RPC response: ${reason}` });
  if (!isObject(body) || body.jsonrpc !== '2.0') return invalid('expected a JSON-RPC 2.0 object');
  if (!Object.hasOwn(body, 'id') || typeof body.id !== typeof requestId || body.id !== requestId) return invalid('request ID mismatch');
  if (Object.hasOwn(body, 'method') || Object.hasOwn(body, 'params')) return invalid('response contains request fields');
  const hasResult = Object.hasOwn(body, 'result');
  const hasError = Object.hasOwn(body, 'error');
  if (hasResult === hasError) return invalid('expected exactly one result or error');
  if (hasError) {
    if (!isObject(body.error) || !Number.isSafeInteger(body.error.code) || typeof body.error.message !== 'string') return invalid('malformed error object');
    return { ok: false, error: body.error };
  }
  return { ok: true, result: body.result };
}
function parseFeature(account) {
  if (account === null) return { status: 'absent', executionAvailability: 'unknown' };
  let b;
  try { b = accountBytes(account); }
  catch { return { status: 'unrecognized', executionAvailability: 'unknown' }; }
  if (account.executable || account.owner !== FEATURE_OWNER || ![1, 9].includes(b.length) || b[0] > 1 ||
      (b[0] === 1 && b.length !== 9)) return { status: 'unrecognized', executionAvailability: 'unknown' };
  return { status: b[0] === 1 ? 'activation_record_present' : 'activation_pending',
    activationSlot: b[0] === 1 ? b.readBigUInt64LE(1).toString() : null,
    executionAvailability: 'unknown_until_same_endpoint_simulation' };
}
export async function subscriptions(url, f, timeoutMs) {
  if (!url) return { status: 'untested', reason: 'No explicit WebSocket endpoint supplied.' };
  return new Promise(resolve => {
    const requests = [
      { method: 'slotSubscribe', params: [] },
      { method: 'accountSubscribe', params: [f?.config.queue ?? CLOCK, { encoding: 'base64', commitment: 'confirmed' }] },
      { method: 'programSubscribe', params: [f?.config.consumerProgram ?? 'Sysvar1111111111111111111111111111111111111',
        { encoding: 'base64', commitment: 'confirmed' }] },
    ];
    const observations = requests.map(x => ({ method: x.method, status: 'no_ack', notifications: 0 }));
    const pending = new Map(requests.map((_, i) => [i + 1, i]));
    const notificationMethods = ['slotNotification', 'accountNotification', 'programNotification'];
    const notifications = [];
    const protocolErrors = [];
    let protocolErrorCount = 0;
    const protocolFailure = reason => {
      protocolErrorCount++;
      if (protocolErrors.length < 20) protocolErrors.push(reason);
    };
    let socket;
    let finished = false;
    const finish = status => {
      if (finished) return;
      finished = true; clearTimeout(timer);
      for (const message of notifications) {
        const i = observations.findIndex((x, i) => x.status === 'acknowledged' &&
          x.subscription === message.params.subscription && notificationMethods[i] === message.method);
        if (i >= 0) {
          observations[i].notifications++;
          observations[i].lastContextSlot = message.params.result?.context?.slot ?? message.params.result?.slot ?? null;
        } else protocolFailure('notification does not match an acknowledged subscription and method');
      }
      if (socket?.readyState === WebSocket.OPEN) for (const [i, observation] of observations.entries()) {
        if (observation.status === 'acknowledged') socket.send(JSON.stringify({ jsonrpc: '2.0', id: i + 10,
          method: requests[i].method.replace('Subscribe', 'Unsubscribe'), params: [observation.subscription] }));
      }
      socket?.close();
      resolve({ status: status === 'window_complete' && protocolErrorCount ? 'protocol_errors' : status,
        observations, protocolErrors, protocolErrorCount, observationWindowMs: timeoutMs,
        interpretation: 'Acknowledgment is not delivery, reliability, recovery, or transaction-driven ER notification qualification.' });
    };
    const timer = setTimeout(() => finish('window_complete'), timeoutMs);
    try {
      socket = new WebSocket(url);
      socket.onopen = () => requests.forEach((request, i) => socket.send(JSON.stringify({ jsonrpc: '2.0', id: i + 1, ...request })));
      socket.onmessage = event => {
        if (finished) return;
        try {
          const message = JSON.parse(String(event.data));
          if (isObject(message) && Object.hasOwn(message, 'id')) {
            const i = pending.get(message.id);
            if (typeof message.id !== 'number' || !Number.isSafeInteger(message.id) || i === undefined) {
              protocolFailure('response ID does not match an outstanding numeric request');
              return;
            }
            pending.delete(message.id);
            const observation = observations[i];
            const reply = rpcResponse(message, i + 1);
            if (!reply.ok) {
              Object.assign(observation, { status: reply.protocolError ? 'invalid_response' : 'rejected', error: reply.error });
              if (reply.protocolError) protocolFailure(reply.error);
            } else if (!Number.isSafeInteger(reply.result) || reply.result < 0 ||
                observations.some(x => x.status === 'acknowledged' && x.subscription === reply.result)) {
              Object.assign(observation, { status: 'invalid_response', error: 'invalid or duplicate subscription ID' });
              protocolFailure(observation.error);
            } else Object.assign(observation, { status: 'acknowledged', subscription: reply.result });
          } else if (isObject(message) && message.jsonrpc === '2.0' && notificationMethods.includes(message.method) &&
              !Object.hasOwn(message, 'result') && !Object.hasOwn(message, 'error') && isObject(message.params) &&
              Number.isSafeInteger(message.params.subscription) && message.params.subscription >= 0 && Object.hasOwn(message.params, 'result')) {
            if (notifications.length < 1000) notifications.push(message);
            else protocolFailure('notification limit exceeded');
          } else protocolFailure('invalid notification or response envelope');
        } catch { finish('invalid_json'); }
      };
      socket.onerror = () => finish('transport_error');
      socket.onclose = () => finish('closed');
    } catch { finish('transport_error'); }
  });
}
export async function runProbe({ rpcUrl, wsUrl, config, targetKind = 'unknown', timeoutMs = 5000 }) {
  if (!['er', 'l1', 'unknown'].includes(targetKind)) throw new Error('target-kind must be er, l1, or unknown');
  const parsed = new URL(rpcUrl);
  if (!['http:', 'https:'].includes(parsed.protocol)) throw new Error('RPC requires HTTP(S)');
  if (!Number.isFinite(timeoutMs) || timeoutMs < 1 || timeoutMs > 60000) throw new Error('timeout-ms must be 1..60000');
  const f = config ? fixture(config) : null;
  let id = 0;
  const rpc = async (method, params = []) => {
    const requestId = ++id;
    const response = await fetch(rpcUrl, { method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: requestId, method, params }), signal: AbortSignal.timeout(timeoutMs) });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return rpcResponse(await response.json(), requestId);
  };
  const safe = async (method, params) => {
    try { return await rpc(method, params); } catch (error) { return { ok: false, error: error.name === 'Error' ? error.message : error.name }; }
  };
  const getAccount = address => safe('getAccountInfo', [address, { encoding: 'base64', commitment: 'confirmed' }]);
  const report = { schema: 'northstar-vrf-runtime-probe-v1', timestamp: new Date().toISOString(),
    endpoint: { host: parsed.host, urlSha256: hash(Buffer.from(rpcUrl)).toString('hex'), operatorAssertedKind: targetKind,
      identityVerified: false },
    scope: 'Read-only observations and unsigned simulations on this endpoint only; no transactions submitted.',
    productionAcceptance: 'not_established', rpc: {}, slots: {}, sysvars: {}, features: {}, simulations: [] };
  for (const method of ['getVersion', 'getGenesisHash', 'getHealth']) report.rpc[method] = await safe(method);
  for (const commitment of ['processed', 'confirmed', 'finalized']) report.slots[commitment] = await safe('getSlot', [{ commitment }]);
  for (const [name, address, parse] of [['clock', CLOCK, parseClock], ['slotHashes', SLOT_HASHES, parseSlotHashes]]) {
    const observation = await getAccount(address);
    report.sysvars[name] = observation;
    if (observation.ok) {
      try { report.sysvars[name] = { context: observation.result.context, parsed: parse(observation.result.value) }; }
      catch (error) { report.sysvars[name] = { status: 'unavailable_or_unrecognized', reason: error.message }; }
    }
  }
  for (const [name, address] of Object.entries(FEATURES)) {
    const observation = await getAccount(address);
    report.features[name] = observation;
    if (observation.ok) {
      try {
        if (!isObject(observation.result) || !Object.hasOwn(observation.result, 'value')) throw new Error('malformed feature account result');
        report.features[name] = { context: observation.result.context, ...parseFeature(observation.result.value) };
      } catch (error) {
        report.features[name] = { status: 'unavailable_or_unrecognized', reason: error.message, executionAvailability: 'unknown' };
      }
    }
  }
  if (f) {
    const addresses = [f.config.probeProgram, f.config.consumerProgram, f.config.queue, f.config.consumerState, f.config.payer];
    const observation = await safe('getMultipleAccounts', [addresses, { encoding: 'base64', commitment: 'confirmed' }]);
    report.fixture = { ...f.config, authority: f.authority, nonce: f.nonce, precondition: 'failed',
      deployedElfBinding: 'Operator must bind these program addresses to the built fixture ELF; not verified by this client.' };
    try {
      if (!observation.ok) throw new Error('getMultipleAccounts failed');
      const [program, consumer, queue, state, payer] = observation.result.value;
      if (!program?.executable || !consumer?.executable) throw new Error('fixture programs are absent or non-executable');
      if (!payer || payer.lamports <= 0 || payer.executable || payer.owner !== '11111111111111111111111111111111') throw new Error('funded system payer required');
      if (queue?.owner !== f.config.probeProgram || state?.owner !== f.config.consumerProgram || queue.executable || state.executable ||
          !accountBytes(queue).equals(f.queueData) || !accountBytes(state).equals(f.stateData)) throw new Error('fixture account initial images differ');
      report.fixture.precondition = 'passed'; report.fixture.context = observation.result.context;
      for (const test of cases(f)) {
        const response = await safe('simulateTransaction', [simulationTransaction(f.config.payer, test.instruction),
          { encoding: 'base64', sigVerify: false, replaceRecentBlockhash: true, commitment: 'confirmed', innerInstructions: true,
            accounts: { encoding: 'base64', addresses: [f.config.queue, f.config.consumerState] } }]);
        const value = response.ok ? response.result?.value : null;
        const entry = { case: test.name, expectedError: test.expected, response, passed: Boolean(value) && JSON.stringify(value.err) === JSON.stringify(test.expected),
          effects: 'unobserved', cuMetadata: Number.isFinite(value?.unitsConsumed) ? 'observed' : 'unavailable' };
        if (entry.passed && test.name === 'callback_success') {
          try {
            const [postQueue, postState] = value.accounts;
            const q = accountBytes(postQueue); const s = accountBytes(postState);
            const expectedQueue = Buffer.from(f.queueData); expectedQueue[40] = 0;
            const expectedState = concat(key(f.config.probeProgram), u64(7), u64(1), s.subarray(48, 56));
            entry.observedCallbackClockSlot = s.length === 56 ? s.readBigUInt64LE(48).toString() : null;
            entry.passed = q.equals(expectedQueue) && s.equals(expectedState) && s.length === 56 && s.readBigUInt64LE(48) > 0n &&
              [[queue, postQueue], [state, postState]].every(([before, after]) => before.owner === after.owner &&
                before.lamports === after.lamports && before.executable === after.executable);
            entry.effects = entry.passed ? 'expected_simulated_images' : 'unexpected_simulated_images';
          } catch { entry.passed = false; entry.effects = 'post_images_unavailable'; }
        } else if (test.expected) entry.effects = 'Rejection only; simulation does not establish committed rollback.';
        report.simulations.push(entry);
      }
    } catch (error) { report.fixture.reason = error.message; }
  }
  report.subscriptions = await subscriptions(wsUrl, f, timeoutMs);
  report.slots.processedAfter = await safe('getSlot', [{ commitment: 'processed' }]);
  report.fixtureSimulation = !f ? 'untested_no_fixture_config' : report.fixture.precondition !== 'passed' ? 'blocked_preconditions' :
    report.simulations.length === cases(f).length && report.simulations.every(x => x.passed) ? 'passed' : 'failed';
  const curve = report.simulations.find(x => x.case === 'curve_known_answers');
  report.curveExecution = !curve ? 'untested' : curve.passed ? 'same_endpoint_fixture_succeeded' : 'same_endpoint_fixture_failed_cause_not_diagnosed';
  report.remaining = [
    'Endpoint ER identity and session binding require operator evidence; backing L1 data alone is insufficient.',
    'No deployed ELF hash/upgrade authority identity, real VRF verification, application ABI, delegated account policy, Portal flow, settlement, history replay, or execution proofs tested.',
    'No committed callback/fee effects, request-to-callback latency, subscription reconnect/reconciliation, or notification reliability tested.',
    'Clock and RPC slots may use different domains; no L1/ER equality, fixed slot duration, wall-clock TTL, or empty-block progress assumed.',
    'MSM size/invalid-curve-ID boundary behavior and all non-Ristretto curve operations remain untested.',
  ];
  return report;
}

if (import.meta.main) {
  try {
    const args = process.argv.slice(2);
    if (args.includes('--help') || !args.length) {
      console.log('bun northstar/scripts/vrf-runtime-rpc.mjs --rpc URL [--ws URL] [--target-kind er|l1|unknown] [--config JSON] [--output JSON] [--timeout-ms 5000]\n--print-fixture emits public fixture account images; it performs no RPC. No signing or transaction submission.');
    } else if (args[0] === '--print-fixture') {
      const f = fixture(args[1] ? JSON.parse(readFileSync(args[1], 'utf8')) : {});
      console.log(JSON.stringify({ config: f.config, authority: f.authority, nonce: f.nonce,
        accounts: [[f.config.queue, f.config.probeProgram, f.queueData], [f.config.consumerState, f.config.consumerProgram, f.stateData]].map(([pubkey, owner, bytes]) =>
          ({ pubkey, account: { lamports: 10000000, owner, executable: false, rentEpoch: 0, data: [bytes.toString('base64'), 'base64'] } })) }, null, 2));
    } else {
      const options = {};
      const allowed = new Set(['--rpc', '--ws', '--target-kind', '--config', '--output', '--timeout-ms']);
      for (let i = 0; i < args.length; i += 2) {
        if (!allowed.has(args[i]) || !args[i + 1] || args[i + 1].startsWith('--')) throw new Error('invalid arguments; use --help');
        options[args[i]] = args[i + 1];
      }
      const report = await runProbe({ rpcUrl: options['--rpc'], wsUrl: options['--ws'], targetKind: options['--target-kind'],
        config: options['--config'] ? JSON.parse(readFileSync(options['--config'], 'utf8')) : null,
        timeoutMs: Number(options['--timeout-ms'] ?? 5000) });
      const output = JSON.stringify(report, null, 2) + '\n';
      if (options['--output']) writeFileSync(options['--output'], output); else console.log(output);
      if (['failed', 'blocked_preconditions'].includes(report.fixtureSimulation)) process.exitCode = 1;
    }
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
