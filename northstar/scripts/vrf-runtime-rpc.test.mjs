import { afterEach, describe, expect, test } from 'bun:test';
import { authority, base58, cases, CLOCK, FEATURES, fixture, parseClock, runProbe,
  simulationTransaction, SLOT_HASHES, u64, unbase58 } from './vrf-runtime-rpc.mjs';

const system = '11111111111111111111111111111111';
const sysvar = 'Sysvar1111111111111111111111111111111111111';
const cat = (...parts) => Buffer.concat(parts.map(x => Buffer.from(x)));
const account = (owner, data, executable = false) => ({ owner, data: [Buffer.from(data).toString('base64'), 'base64'],
  executable, lamports: 10000000, rentEpoch: 0 });
const servers = [];
afterEach(() => { for (const server of servers.splice(0)) server.stop(true); });

const invalidReplies = [
  ['mismatched ID', reply => ({ ...reply, id: 999999 })],
  ['missing ID', ({ id, ...reply }) => reply],
  ['string ID', reply => ({ ...reply, id: String(reply.id) })],
  ['boolean ID', reply => ({ ...reply, id: true })],
  ['null ID', reply => ({ ...reply, id: null })],
  ['fractional ID', reply => ({ ...reply, id: reply.id + 0.5 })],
  ['invalid version', reply => ({ ...reply, jsonrpc: '1.0' })],
  ['missing version', ({ jsonrpc, ...reply }) => reply],
  ['null envelope', () => null],
  ['array envelope', reply => [reply]],
  ['primitive envelope', () => 'ok'],
  ['missing result and error', reply => ({ jsonrpc: reply.jsonrpc, id: reply.id })],
  ['both result and error', reply => ({ ...reply, result: reply.result ?? null, error: { code: -32603, message: 'test' } })],
  ['null error', reply => ({ jsonrpc: reply.jsonrpc, id: reply.id, error: null })],
  ['invalid error code type', reply => ({ jsonrpc: reply.jsonrpc, id: reply.id, error: { code: '-32603', message: 'test' } })],
  ['invalid error message type', reply => ({ jsonrpc: reply.jsonrpc, id: reply.id, error: { code: -32603, message: true } })],
  ['request-shaped envelope', reply => ({ ...reply, method: 'getHealth', params: [] })],
];

function mock({ wrongPost = false, wrongInitial = false, omitUnits = false, rejectSimulation = false,
  httpResponse = reply => reply, wsResponse = reply => reply, featureResponse = (_address, result) => result } = {}) {
  const f = fixture();
  const queue = account(f.config.probeProgram, f.queueData);
  const state = account(f.config.consumerProgram, f.stateData);
  const tests = cases(f);
  const transactions = new Map(tests.map(test => [simulationTransaction(f.config.payer, test.instruction), test]));
  const calls = [];
  const server = Bun.serve({ hostname: '127.0.0.1', port: 0,
    async fetch(request, server) {
      if (request.headers.get('upgrade') === 'websocket') { server.upgrade(request); return; }
      const body = await request.json(); calls.push(body);
      let result;
      let error;
      switch (body.method) {
        case 'getVersion': result = { 'solana-core': 'mock-not-an-ER', 'feature-set': 0 }; break;
        case 'getGenesisHash': result = system; break;
        case 'getHealth': result = 'ok'; break;
        case 'getSlot': result = 10; break;
        case 'getAccountInfo': {
          const address = body.params[0];
          const data = address === CLOCK ? account(sysvar, cat(u64(9), Buffer.alloc(32))) :
            address === SLOT_HASHES ? account(sysvar, cat(u64(1), u64(8), Buffer.alloc(32))) : null;
          result = { context: { slot: 10 }, value: data };
          if (Object.values(FEATURES).includes(address)) result = featureResponse(address, result);
          break;
        }
        case 'getMultipleAccounts': result = { context: { slot: 10 }, value: [
          account(system, [], true), account(system, [], true), wrongInitial ? account(system, []) : queue,
          state, account(system, []),
        ] }; break;
        case 'simulateTransaction': {
          if (rejectSimulation) { error = { code: -32601, message: 'simulation unsupported' }; break; }
          const test = transactions.get(body.params[0]);
          if (!test) throw new Error('unexpected transaction bytes');
          const q = Buffer.from(f.queueData); q[40] = 0;
          const s = cat(unbase58(f.config.probeProgram), u64(wrongPost ? 8 : 7), u64(1), u64(9));
          result = { context: { slot: 10 }, value: { err: test.expected, logs: [],
            accounts: test.expected ? null : test.name === 'callback_success' ?
              [account(f.config.probeProgram, q), account(f.config.consumerProgram, s)] : [queue, state],
            ...(!omitUnits ? { unitsConsumed: 1234 } : {}), innerInstructions: [] } }; break;
        }
        default: throw new Error(`unexpected RPC ${body.method}`);
      }
      return Response.json(httpResponse({ jsonrpc: '2.0', id: body.id, ...(error ? { error } : { result }) }, body));
    },
    websocket: { message(socket, text) {
      const body = JSON.parse(String(text));
      if (body.method.endsWith('Unsubscribe')) return;
      const send = reply => socket.send(JSON.stringify(wsResponse(reply, body)));
      if (body.method === 'programSubscribe') {
        send({ jsonrpc: '2.0', id: body.id, error: { code: -32601, message: 'unsupported' } });
      } else {
        send({ jsonrpc: '2.0', id: body.id, result: body.id + 100 });
        if (body.method === 'slotSubscribe') send({ jsonrpc: '2.0', method: 'slotNotification',
          params: { subscription: body.id + 100, result: { slot: 10, parent: 9, root: 8 } } });
      }
    } },
  });
  servers.push(server);
  return { f, calls, rpcUrl: `http://127.0.0.1:${server.port}`, wsUrl: `ws://127.0.0.1:${server.port}` };
}

describe('public fixture encoding', () => {
  test('base58 and PDA match Rust fixtures including a non-255 bump', () => {
    expect(base58(Buffer.alloc(32))).toBe(system);
    for (let i = 0; i < 100; i++) expect(unbase58(base58(Buffer.alloc(32, i)))).toEqual(Buffer.alloc(32, i));
    const f = fixture();
    expect(f.authority).toEqual({ address: 'AMkbnjBBkuPDdc17TgybYQRQ69SDv3LCVXHD7tKDAeTk', bump: 255 });
    expect(authority(f.config.probeProgram, base58(Buffer.alloc(32, 1))))
      .toEqual({ address: '2ntPdWBrek8nFxkwg5dXZZZFTXttRUKjPyeRVk9VTFhN', bump: 253 });
    expect(() => fixture({ payer: f.config.queue })).toThrow('aliases');
    expect(() => fixture({ config: f.config })).toThrow('unknown fixture config field');
    expect(() => fixture([])).toThrow('fixture config must be an object');
    expect(() => unbase58('0OIl')).toThrow();
  });
  test('legacy transaction carries only zero signature and expected privileges', () => {
    const f = fixture();
    const bytes = Buffer.from(simulationTransaction(f.config.payer, cases(f)[1].instruction), 'base64');
    expect(bytes[0]).toBe(1);
    expect(bytes.subarray(1, 65)).toEqual(Buffer.alloc(64));
    expect([...bytes.subarray(65, 68)]).toEqual([1, 0, 4]);
    expect(bytes[68]).toBe(7);
    const keys = Array.from({ length: 7 }, (_, i) => base58(bytes.subarray(69 + i * 32, 101 + i * 32)));
    expect(keys.slice(0, 3)).toEqual([f.config.payer, f.config.queue, f.config.consumerState]);
    expect(keys.slice(3)).toEqual([f.authority.address, f.config.consumerProgram, CLOCK, f.config.probeProgram]);
    const offset = 69 + 7 * 32 + 32;
    expect([...bytes.subarray(offset, offset + 8)]).toEqual([1, 6, 5, 1, 2, 3, 4, 5]);
    expect(bytes.length).toBeLessThan(1233);
  });
  test('Clock uses integer strings without conflating slot domains', () => {
    const slot = (1n << 63n) + 1n;
    expect(parseClock(account(sysvar, cat(u64(slot), Buffer.alloc(32)))).slot).toBe(slot.toString());
    expect(() => parseClock(account(sysvar, []))).toThrow();
  });
});

describe('read-only endpoint runner against a mock, not an ER', () => {
  test('checks post-images, expected rejections, CU and subscription distinctions', async () => {
    const m = mock();
    const report = await runProbe({ ...m, config: m.f.config, targetKind: 'er', timeoutMs: 50 });
    expect(report.fixtureSimulation).toBe('passed');
    expect(report.simulations).toHaveLength(11);
    expect(report.simulations.every(x => x.passed)).toBe(true);
    expect(report.simulations.find(x => x.case === 'callback_success').effects).toBe('expected_simulated_images');
    expect(report.features.curve25519.status).toBe('absent');
    expect(report.features.curve25519.executionAvailability).toBe('unknown');
    expect(report.endpoint.identityVerified).toBe(false);
    expect(report.productionAcceptance).toBe('not_established');
    const [slot, queue, program] = report.subscriptions.observations;
    expect(slot.status).toBe('acknowledged'); expect(slot.notifications).toBe(1);
    expect(queue.status).toBe('acknowledged'); expect(queue.notifications).toBe(0);
    expect(program.status).toBe('rejected');
    expect(m.calls.every(x => x.method !== 'sendTransaction')).toBe(true);
    for (const call of m.calls.filter(x => x.method === 'simulateTransaction')) {
      expect(call.params[1].sigVerify).toBe(false);
      expect(call.params[1].replaceRecentBlockhash).toBe(true);
      expect(call.params[1].innerInstructions).toBe(true);
    }
    expect(m.calls.filter(x => x.method === 'getAccountInfo').map(x => x.params[0])).toEqual([CLOCK, SLOT_HASHES, ...Object.values(FEATURES)]);
  });
  test('RPC identity/feature observations alone do not qualify execution', async () => {
    const m = mock();
    const report = await runProbe({ rpcUrl: m.rpcUrl, timeoutMs: 50 });
    expect(report.fixtureSimulation).toBe('untested_no_fixture_config');
    expect(report.simulations).toEqual([]);
    expect(report.subscriptions.status).toBe('untested');
    expect(report.endpoint.operatorAssertedKind).toBe('unknown');
  });
  test('incorrect fixture image blocks simulation', async () => {
    const m = mock({ wrongInitial: true });
    const report = await runProbe({ rpcUrl: m.rpcUrl, config: m.f.config, timeoutMs: 50 });
    expect(report.fixtureSimulation).toBe('blocked_preconditions');
    expect(m.calls.some(x => x.method === 'simulateTransaction')).toBe(false);
  });
  test('successful error code without expected post-image fails conformance', async () => {
    const m = mock({ wrongPost: true });
    const report = await runProbe({ rpcUrl: m.rpcUrl, config: m.f.config, timeoutMs: 50 });
    expect(report.fixtureSimulation).toBe('failed');
    expect(report.simulations.find(x => x.case === 'callback_success').passed).toBe(false);
  });
  test('missing CU is reported rather than invented', async () => {
    const m = mock({ omitUnits: true });
    const report = await runProbe({ rpcUrl: m.rpcUrl, config: m.f.config, timeoutMs: 50 });
    expect(report.fixtureSimulation).toBe('passed');
    expect(report.simulations.every(x => x.cuMetadata === 'unavailable')).toBe(true);
  });
  test('unsupported simulation cannot pass', async () => {
    const m = mock({ rejectSimulation: true });
    const report = await runProbe({ rpcUrl: m.rpcUrl, config: m.f.config, timeoutMs: 50 });
    expect(report.fixtureSimulation).toBe('failed');
    expect(report.simulations.every(x => !x.passed)).toBe(true);
  });
});

describe('JSON-RPC observation correlation invariants', () => {
  test.each(invalidReplies)('HTTP %s cannot establish health, preconditions or simulation success', async (_name, change) => {
    const m = mock({ httpResponse: change });
    const report = await runProbe({ rpcUrl: m.rpcUrl, config: m.f.config, timeoutMs: 100 });
    expect(report.rpc.getHealth.ok).toBe(false);
    expect(report.rpc.getHealth.protocolError).toBe(true);
    expect(report.fixtureSimulation).toBe('blocked_preconditions');
    expect(report.simulations).toEqual([]);
    expect(report.productionAcceptance).toBe('not_established');

    const s = mock({ httpResponse: (reply, request) => request.method === 'simulateTransaction' ? change(reply) : reply });
    const simulations = await runProbe({ rpcUrl: s.rpcUrl, config: s.f.config, timeoutMs: 100 });
    expect(simulations.fixture.precondition).toBe('passed');
    expect(simulations.fixtureSimulation).toBe('failed');
    expect(simulations.simulations).toHaveLength(11);
    expect(simulations.simulations.every(x => !x.passed && !x.response.ok && x.response.protocolError)).toBe(true);
    expect(simulations.curveExecution).toBe('same_endpoint_fixture_failed_cause_not_diagnosed');
  });

  test.each(invalidReplies)('WebSocket %s cannot acknowledge an outstanding request', async (_name, change) => {
    const m = mock({ wsResponse: reply => Object.hasOwn(reply, 'id') ? change(reply) : reply });
    const report = await runProbe({ ...m, timeoutMs: 50 });
    expect(report.subscriptions.status).toBe('protocol_errors');
    expect(report.subscriptions.protocolErrors.length).toBeGreaterThan(0);
    expect(report.subscriptions.observations.every(x => x.status !== 'acknowledged' && x.notifications === 0)).toBe(true);
  });

  test.each([
    ['wrong method', reply => ({ ...reply, method: 'accountNotification' })],
    ['string subscription ID', reply => ({ ...reply, params: { ...reply.params, subscription: String(reply.params.subscription) } })],
    ['wrong version', reply => ({ ...reply, jsonrpc: '1.0' })],
    ['notification with ID', reply => ({ ...reply, id: 1 })],
  ])('WebSocket %s cannot count as a correlated slot notification', async (_name, change) => {
    const m = mock({ wsResponse: reply => Object.hasOwn(reply, 'method') ? change(reply) : reply });
    const report = await runProbe({ ...m, timeoutMs: 50 });
    expect(report.subscriptions.status).toBe('protocol_errors');
    expect(report.subscriptions.protocolErrors.length).toBeGreaterThan(0);
    expect(report.subscriptions.observations[0].notifications).toBe(0);
  });

  test.each([
    ['null account result', () => null],
    ['missing account value', () => ({ context: { slot: 10 } })],
    ['invalid encoded account', () => ({ context: { slot: 10 }, value: { owner: 'Feature111111111111111111111111111111111111', data: [null, 'base64'] } })],
    ['non-string encoding', () => ({ context: { slot: 10 }, value: { owner: 'Feature111111111111111111111111111111111111', data: [[0], 'base64'] } })],
    ['non-canonical base64', () => ({ context: { slot: 10 }, value: { owner: 'Feature111111111111111111111111111111111111', data: ['AA==?', 'base64'] } })],
  ])('malformed feature %s is explicit without losing the endpoint report', async (_name, change) => {
    const m = mock({ featureResponse: (address, result) => address === FEATURES.curve25519 ? change() : result });
    const report = await runProbe({ rpcUrl: m.rpcUrl, config: m.f.config, timeoutMs: 100 });
    expect(report.rpc.getHealth).toEqual({ ok: true, result: 'ok' });
    expect(report.features.curve25519.status).toMatch(/unrecognized|unavailable/);
    expect(report.features.curve25519.executionAvailability).toBe('unknown');
    expect(report.features.msm_length_compatibility.status).toBe('absent');
    expect(report.slots.processedAfter.ok).toBe(true);
    expect(report.fixtureSimulation).toBe('passed');
    expect(report.productionAcceptance).toBe('not_established');
  });
});
