// Sonic: Check retry and retained-state invariants without chain access or signing keys.
import { test, expect } from 'bun:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { ControlJournal } from './continuity-control-journal.mjs';

const binding = { genesis: 'devnet', portal: 'isolated-program', profile: '01'.repeat(32) };
const wire = Buffer.from(JSON.stringify({ signature: 'original-signature', blockhash: 'original-blockhash' }));
const decode = value => JSON.parse(value.toString());
function fixture(check = async () => {}) {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'continuity-controls-'));
  const root = path.join(temporary, 'journal');
  const calls = [];
  const rpc = {
    getLatestBlockhash: async () => ({ blockhash: 'original-blockhash', lastValidBlockHeight: 100 }),
    sendRawTransaction: async data => { calls.push(Buffer.from(data)); await rpc.onSend?.(data); return 'original-signature'; },
    getSignatureStatuses: async () => ({ value: [{ slot: 10, err: null, confirmationStatus: 'finalized' }] }),
    isBlockhashValid: async () => ({ value: true }),
  };
  const open = () => {
    const journal = new ControlJournal(root, binding, decode, check, { timeoutMs: 150, pollMs: 1 });
    journal.attach('l1', rpc);
    return journal;
  };
  const journal = open();
  const send = async () => { await rpc.getLatestBlockhash(); return rpc.sendRawTransaction(wire); };
  return { temporary, root, rpc, calls, journal, open, send, close: () => fs.rmSync(temporary, { recursive: true, force: true }) };
}

test('signed bytes are durable before the first submission and reopen without signing', async () => {
  const value = fixture();
  try {
    value.rpc.onSend = async () => {
      const intent = JSON.parse(fs.readFileSync(path.join(value.root, 'deposit/intent.json')));
      expect(Buffer.from(intent.wire_base64, 'base64').equals(wire)).toBe(true);
    };
    const sent = await value.journal.run('deposit', 'l1', { amount: '50' }, value.send);
    expect(sent.signature).toBe('original-signature');
    expect(value.calls.length).toBe(1);
    const again = await value.open().run('deposit', 'l1', { amount: '50' }, () => { throw new Error('must not sign'); });
    expect(again).toEqual(sent);
    expect(value.calls.length).toBe(1);
  } finally { value.close(); }
});

test('new journal roots flush their parent directory before use', () => {
  const value = fixture();
  const original = fs.fsyncSync;
  const parent = fs.statSync(value.temporary).ino;
  let published = false;
  try {
    fs.fsyncSync = descriptor => {
      const info = fs.fstatSync(descriptor);
      if (info.isDirectory() && info.ino === parent) published = true;
      return original(descriptor);
    };
    new ControlJournal(path.join(value.temporary, 'new-root'), binding, decode, async () => {});
    expect(published).toBe(true);
  } finally { fs.fsyncSync = original; value.close(); }
});

test('changed operation parameters cannot reuse a saved receipt', async () => {
  const value = fixture();
  try {
    await value.journal.run('deposit', 'l1', { amount: '50' }, value.send);
    await expect(value.journal.run('deposit', 'l1', { amount: '51' }, value.send)).rejects.toThrow('changed');
    expect(value.calls.length).toBe(1);
  } finally { value.close(); }
});

test('unknown submission rebroadcasts identical bytes without a replacement signature', async () => {
  const value = fixture();
  try {
    let submissions = 0;
    const rawRpc = { ...value.rpc,
      sendRawTransaction: async data => {
        value.calls.push(Buffer.from(data));
        if (++submissions === 1) throw new Error('response unavailable');
        return 'original-signature';
      },
      getSignatureStatuses: async () => ({ value: submissions > 1 ? [{ slot: 10, err: null, confirmationStatus: 'finalized' }] : [null] }),
    };
    const journal = new ControlJournal(path.join(value.temporary, 'retry'), binding, decode, async () => {}, { timeoutMs: 200, pollMs: 1 });
    journal.attach('l1', rawRpc);
    await journal.run('deposit', 'l1', {}, async () => { await rawRpc.getLatestBlockhash(); await rawRpc.sendRawTransaction(wire); });
    expect(value.calls.length).toBe(2);
    expect(value.calls[0].equals(value.calls[1])).toBe(true);
  } finally { value.close(); }
});

test('missing saved intent cannot be hidden by a completion receipt', async () => {
  const value = fixture();
  try {
    await value.journal.run('deposit', 'l1', {}, value.send);
    fs.unlinkSync(path.join(value.root, 'deposit/intent.json'));
    await expect(value.open().run('deposit', 'l1', {}, value.send)).rejects.toThrow();
  } finally { value.close(); }
});

test('retained observation prevents signing after saved-intent loss', async () => {
  const value = fixture();
  try {
    await value.journal.run('deposit', 'l1', {}, value.send);
    fs.unlinkSync(path.join(value.root, 'deposit/receipt.json'));
    fs.unlinkSync(path.join(value.root, 'deposit/intent.json'));
    let attempts = 0;
    await expect(value.open().run('deposit', 'l1', {}, async () => { attempts++; throw new Error('must not sign'); })).rejects.toThrow();
    expect(attempts).toBe(0);
    expect(value.calls.length).toBe(1);
  } finally { value.close(); }
});

test('a saved intent cannot satisfy a different semantic operation', async () => {
  const value = fixture();
  try {
    await value.journal.run('deposit', 'l1', { recipient: 'first' }, value.send);
    await expect(value.journal.run('other-deposit', 'l1', { recipient: 'second' }, async () => { throw new Error('not signed'); })).rejects.toThrow();
    for (const name of ['intent.json', 'observed.json', 'receipt.json']) {
      fs.copyFileSync(path.join(value.root, 'deposit', name), path.join(value.root, 'other-deposit', name));
    }
    await expect(value.journal.run('other-deposit', 'l1', { recipient: 'second' }, value.send)).rejects.toThrow();
    expect(value.calls.length).toBe(1);
  } finally { value.close(); }
});

test('changed completion signature is rejected before another submission', async () => {
  const value = fixture();
  try {
    await value.journal.run('deposit', 'l1', {}, value.send);
    const record = path.join(value.root, 'deposit/receipt.json');
    const data = JSON.parse(fs.readFileSync(record));
    fs.writeFileSync(record, JSON.stringify({ ...data, signature: 'different' }), { mode: 0o600 });
    await expect(value.open().run('deposit', 'l1', {}, value.send)).rejects.toThrow();
    expect(value.calls.length).toBe(1);
  } finally { value.close(); }
});

test('changed binding and orphaned established state never initialize a fresh journal', async () => {
  const value = fixture();
  try {
    expect(() => new ControlJournal(value.root, { ...binding, portal: 'another' }, decode, async () => {})).toThrow('changed');
    fs.unlinkSync(path.join(value.root, 'binding.json'));
    fs.mkdirSync(path.join(value.root, 'retained'));
    expect(() => value.open()).toThrow('binding is missing');
  } finally { value.close(); }
});

test('identity drift after saving signed bytes prevents submission', async () => {
  let intent;
  const value = fixture(async () => { if (intent && fs.existsSync(intent)) throw new Error('session identity changed'); });
  intent = path.join(value.root, 'deposit/intent.json');
  try {
    await expect(value.journal.run('deposit', 'l1', {}, value.send)).rejects.toThrow('identity changed');
    expect(fs.existsSync(intent)).toBe(true);
    expect(value.calls.length).toBe(0);
  } finally { value.close(); }
});

test('an operation cannot silently complete after requesting a second transaction', async () => {
  const value = fixture();
  try {
    await expect(value.journal.run('deposit', 'l1', {}, async () => {
      await value.send();
      const second = Buffer.from(JSON.stringify({ signature: 'second-signature', blockhash: 'original-blockhash' }));
      await value.rpc.sendRawTransaction(second);
    })).rejects.toThrow();
    expect(value.calls.length).toBe(1);
    await expect(value.open().run('deposit', 'l1', {}, value.send)).rejects.toThrow();
  } finally { value.close(); }
});

test('no transaction can be sent outside its operation or without observed lifetime', async () => {
  const value = fixture();
  try {
    await expect(value.rpc.sendRawTransaction(wire)).rejects.toThrow('outside');
    await expect(value.journal.run('deposit', 'l1', {}, () => value.rpc.sendRawTransaction(wire))).rejects.toThrow('lifetime');
    expect(value.calls.length).toBe(0);
  } finally { value.close(); }
});

test('recovery floor comes only from bound complete receipts', async () => {
  const value = fixture();
  try {
    expect(value.journal.receiptFloor('l1')).toBe(0);
    await value.journal.run('deposit', 'l1', {}, value.send);
    expect(value.journal.receiptFloor('l1')).toBe(10);
    const filename = path.join(value.root, 'deposit/observed.json');
    const original = JSON.parse(fs.readFileSync(filename));
    fs.writeFileSync(filename, JSON.stringify({ ...original, signature: 'different' }));
    expect(() => value.journal.receiptFloor('l1')).toThrow('binding or integrity');
  } finally { value.close(); }
});

test('checkpoint holds retry only the original ER signed bytes while valid', async () => {
  const value = fixture();
  try {
    let submissions = 0;
    const rawRpc = { ...value.rpc,
      sendRawTransaction: async data => {
        value.calls.push(Buffer.from(data));
        if (++submissions < 3) throw new Error('Simulation failed. canonical checkpoint awaits settlement; retry after settlement.');
        return 'original-signature';
      },
      getSignatureStatuses: async () => ({ value: submissions >= 3 ? [{ slot: 10, err: null, confirmationStatus: 'confirmed' }] : [null] }),
    };
    const journal = new ControlJournal(path.join(value.temporary, 'checkpoint'), binding, decode, async () => {}, { timeoutMs: 200, pollMs: 1 });
    journal.attach('er', rawRpc);
    const result = await journal.run('withdrawal', 'er', {}, async () => { await rawRpc.getLatestBlockhash(); await rawRpc.sendRawTransaction(wire); });
    expect(result.signature).toBe('original-signature');
    expect(value.calls.length).toBe(3);
    expect(value.calls.every(sent => sent.equals(wire))).toBe(true);
  } finally { value.close(); }
});

test('checkpoint backpressure never permits submission after original expiry', async () => {
  const value = fixture();
  try {
    const rawRpc = { ...value.rpc,
      sendRawTransaction: async data => { value.calls.push(Buffer.from(data)); throw new Error('canonical checkpoint awaits settlement; retry after settlement.'); },
      getSignatureStatuses: async () => ({ value: [null] }),
      isBlockhashValid: async () => ({ value: value.calls.length < 3 }),
    };
    const journal = new ControlJournal(path.join(value.temporary, 'expired-checkpoint'), binding, decode, async () => {}, { timeoutMs: 200, pollMs: 1 });
    journal.attach('er', rawRpc);
    await expect(journal.run('withdrawal', 'er', {}, async () => { await rawRpc.getLatestBlockhash(); await rawRpc.sendRawTransaction(wire); })).rejects.toThrow('expired');
    expect(value.calls.length).toBe(3);
    expect(value.calls.every(sent => sent.equals(wire))).toBe(true);
  } finally { value.close(); }
});

test('malformed RPC status is not absence', async () => {
  const value = fixture();
  try {
    value.rpc.getSignatureStatuses = async () => ({ value: [{}] });
    await expect(value.journal.run('deposit', 'l1', {}, value.send)).rejects.toThrow('Invalid control receipt');
    expect(value.calls.length).toBe(1);
  } finally { value.close(); }
});
