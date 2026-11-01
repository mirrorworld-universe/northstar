// Sonic: Preserve exact signed transactions across deployment retries; never replace an established signature.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

class ControlInvariantError extends Error {}
const hash = value => crypto.createHash('sha256').update(value).digest('hex');
const present = filename => fs.lstatSync(filename, { throwIfNoEntry: false }) !== undefined;
function canonical(value) {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return JSON.stringify(value);
  if (typeof value === 'number' && Number.isSafeInteger(value)) return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value && Object.getPrototypeOf(value) === Object.prototype) {
    return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`;
  }
  throw new Error('Control metadata must contain bounded JSON values');
}
export function readPrivateJson(filename) {
  const fd = fs.openSync(filename, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile() || (stat.mode & 0o077) !== 0 || stat.uid !== process.getuid() || stat.size > 16 * 1024 * 1024) {
      throw new Error('Control input must be a private regular file owned by the runner');
    }
    return JSON.parse(fs.readFileSync(fd, 'utf8'));
  } finally { fs.closeSync(fd); }
}
export function atomicPrivateJson(filename, value) {
  if (present(filename)) readPrivateJson(filename);
  const temporary = `${filename}.${crypto.randomUUID()}.tmp`;
  const fd = fs.openSync(temporary, 'wx', 0o600);
  try { fs.writeFileSync(fd, canonical(value) + '\n'); fs.fsyncSync(fd); }
  finally { fs.closeSync(fd); }
  fs.renameSync(temporary, filename);
  const directory = fs.openSync(path.dirname(filename), 'r');
  try { fs.fsyncSync(directory); } finally { fs.closeSync(directory); }
}
function immutable(filename, value) {
  if (present(filename)) {
    if (canonical(readPrivateJson(filename)) !== canonical(value)) throw new ControlInvariantError('Established control metadata changed');
  } else atomicPrivateJson(filename, value);
}
function directory(filename) {
  if (!fs.existsSync(filename)) {
    fs.mkdirSync(filename, { mode: 0o700 });
    const parent = fs.openSync(path.dirname(filename), 'r');
    try { fs.fsyncSync(parent); } finally { fs.closeSync(parent); }
  }
  const stat = fs.lstatSync(filename);
  if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== process.getuid() || (stat.mode & 0o077) !== 0) {
    throw new Error('Control directory must be private and owned by the runner');
  }
}

export class ControlJournal {
  constructor(root, binding, decodeWire, beforeSend, options = {}) {
    this.root = path.resolve(root);
    this.binding = hash(canonical(binding));
    this.decodeWire = decodeWire;
    this.beforeSend = beforeSend;
    this.timeoutMs = options.timeoutMs ?? 180_000;
    this.pollMs = options.pollMs ?? 500;
    this.layers = new Map();
    directory(this.root);
    const established = path.join(this.root, 'binding.json');
    if (!present(established) && fs.readdirSync(this.root).length) throw new Error('Control binding is missing');
    immutable(established, { version: 2, binding: this.binding, identity: binding });
  }

  attach(layer, rpc) {
    if (this.layers.has(layer)) throw new Error('Control RPC layer already attached');
    const send = rpc.sendRawTransaction.bind(rpc);
    const latest = rpc.getLatestBlockhash.bind(rpc);
    const expiries = new Map();
    rpc.getLatestBlockhash = async (...args) => {
      const value = await latest(...args);
      if (!Number.isSafeInteger(value.lastValidBlockHeight) || value.lastValidBlockHeight < 0) throw new Error('Invalid control blockhash lifetime');
      expiries.set(value.blockhash, value.lastValidBlockHeight);
      return value;
    };
    rpc.sendRawTransaction = async (input, options) => {
      if (!this.active || this.active.layer !== layer) throw new Error('Transaction submitted outside its bound operation');
      const wire = Buffer.from(input);
      if (!wire.length || wire.length > 1232) throw new Error('Invalid signed transaction size');
      const decoded = this.decodeWire(wire);
      const expiry = expiries.get(decoded.blockhash);
      if (expiry === undefined) throw new Error('Signed transaction lifetime was not observed');
      await this.beforeSend(this.active.label, layer, { submit: true });
      const intent = {
        version: 2, binding: this.binding, operation_sha256: this.active.headerHash, layer, signature: decoded.signature,
        blockhash: decoded.blockhash, last_valid_block_height: expiry,
        wire_sha256: hash(wire), wire_base64: wire.toString('base64'),
      };
      immutable(path.join(this.active.directory, 'intent.json'), intent);
      await this.beforeSend(this.active.label, layer, { submit: true });
      if (present(path.join(this.active.directory, 'observed.json'))) return intent.signature;
      const result = await send(wire, { ...options, maxRetries: 0 });
      if (result !== intent.signature) throw new Error('Submitted control signature differs');
      return result;
    };
    this.layers.set(layer, { rpc, send });
  }

  readIntent(operation) {
    const intent = readPrivateJson(path.join(operation.directory, 'intent.json'));
    const wire = Buffer.from(intent.wire_base64, 'base64');
    const decoded = this.decodeWire(wire);
    if (intent.version !== 2 || intent.binding !== this.binding || intent.operation_sha256 !== operation.headerHash || intent.layer !== operation.layer ||
        !Number.isSafeInteger(intent.last_valid_block_height) || intent.last_valid_block_height < 0 ||
        !wire.length || wire.length > 1232 || wire.toString('base64') !== intent.wire_base64 ||
        hash(wire) !== intent.wire_sha256 || decoded.signature !== intent.signature || decoded.blockhash !== intent.blockhash) {
      throw new ControlInvariantError('Saved transaction binding or integrity differs');
    }
    return { intent, wire };
  }

  readReceipt(operation) {
    const { intent } = this.readIntent(operation);
    const result = readPrivateJson(path.join(operation.directory, 'receipt.json'));
    const observed = readPrivateJson(path.join(operation.directory, 'observed.json'));
    if (result.signature !== intent.signature || observed.signature !== intent.signature ||
        !Number.isSafeInteger(result.slot) || result.slot < 0 || observed.receipt?.slot !== result.slot ||
        observed.receipt?.err !== null || observed.receipt?.confirmationStatus !== result.confirmation ||
        !(result.confirmation === 'finalized' || (operation.layer === 'er' && result.confirmation === 'confirmed'))) {
      throw new ControlInvariantError('Retained control receipt binding or integrity differs');
    }
    return result;
  }

  receiptFloor(layer) {
    const entries = fs.readdirSync(this.root);
    if (entries.length > 128) throw new ControlInvariantError('Control operation count exceeds bound');
    let floor = 0;
    for (const label of entries) {
      if (label === 'binding.json') continue;
      if (!/^[a-z][a-z0-9-]{0,63}$/.test(label)) throw new ControlInvariantError('Unknown retained control entry');
      const operation = { label, directory: path.join(this.root, label) };
      directory(operation.directory);
      const header = readPrivateJson(path.join(operation.directory, 'operation.json'));
      if (header.version !== 2 || header.binding !== this.binding || header.label !== label || !this.layers.has(header.layer)) {
        throw new ControlInvariantError('Retained operation identity differs');
      }
      operation.layer = header.layer;
      operation.headerHash = hash(canonical(header));
      if (present(path.join(operation.directory, 'fatal.json'))) throw new ControlInvariantError('Retained control contract failure requires review');
      if (present(path.join(operation.directory, 'receipt.json'))) {
        const receipt = this.readReceipt(operation);
        if (operation.layer === layer) floor = Math.max(floor, receipt.slot);
      }
    }
    return floor;
  }

  async confirm(operation, allowRebroadcast) {
    const { intent, wire } = this.readIntent(operation);
    const { rpc, send } = this.layers.get(operation.layer);
    const observedPath = path.join(operation.directory, 'observed.json');
    if (present(observedPath)) {
      const saved = readPrivateJson(observedPath);
      if (saved.signature !== intent.signature || !saved.receipt || !Object.hasOwn(saved.receipt, 'err') ||
          !Number.isSafeInteger(saved.receipt.slot) || saved.receipt.slot < 0 ||
          !['processed', 'confirmed', 'finalized'].includes(saved.receipt.confirmationStatus)) {
        throw new ControlInvariantError('Retained observation binding or integrity differs');
      }
    }
    const deadline = Date.now() + this.timeoutMs;
    let rebroadcast = false;
    while (Date.now() < deadline) {
      await this.beforeSend(operation.label, operation.layer, { submit: false });
      const response = await rpc.getSignatureStatuses([intent.signature], { searchTransactionHistory: true });
      if (!Array.isArray(response?.value) || response.value.length !== 1) throw new Error('Invalid control status response');
      const receipt = response.value[0];
      if (receipt !== null) {
        if (!receipt || !Object.hasOwn(receipt, 'err') || !Number.isSafeInteger(receipt.slot) || receipt.slot < 0 ||
            !['processed', 'confirmed', 'finalized'].includes(receipt.confirmationStatus)) throw new Error('Invalid control receipt');
        atomicPrivateJson(path.join(operation.directory, 'observed.json'), { signature: intent.signature, receipt });
        if (receipt.err !== null) throw new Error('Control transaction was rejected; retained receipt requires review');
        if (receipt.confirmationStatus === 'finalized' || (operation.layer === 'er' && receipt.confirmationStatus === 'confirmed')) {
          const result = { signature: intent.signature, slot: receipt.slot, confirmation: receipt.confirmationStatus };
          immutable(path.join(operation.directory, 'receipt.json'), result);
          return result;
        }
      } else if (allowRebroadcast && !rebroadcast && !present(observedPath)) {
        const valid = await rpc.isBlockhashValid(intent.blockhash, { commitment: 'confirmed' });
        if (typeof valid?.value !== 'boolean') throw new Error('Invalid control blockhash response');
        if (!valid.value) throw new Error('Saved transaction expired without a receipt; no replacement is permitted');
        await this.beforeSend(operation.label, operation.layer, { submit: true });
        try {
          const signature = await send(wire, { skipPreflight: false, maxRetries: 0 });
          if (signature !== intent.signature) throw new Error('Rebroadcast signature differs');
          rebroadcast = true;
        } catch (error) {
          if (operation.layer !== 'er' || !String(error?.message).includes('canonical checkpoint awaits settlement; retry after settlement.')) throw error;
        }
      }
      await new Promise(resolve => setTimeout(resolve, this.pollMs));
    }
    throw new Error('Control receipt timeout; original signed intent retained');
  }

  async run(label, layer, request, action) {
    if (!/^[a-z][a-z0-9-]{0,63}$/.test(label) || !this.layers.has(layer) || this.active) throw new Error('Invalid or concurrent control operation');
    await this.beforeSend(label, layer, { submit: true });
    const operation = { label, layer, directory: path.join(this.root, label) };
    directory(operation.directory);
    const header = path.join(operation.directory, 'operation.json');
    if (!present(header) && fs.readdirSync(operation.directory).length) throw new Error('Established operation metadata is missing');
    const headerValue = { version: 2, binding: this.binding, label, layer, request };
    operation.headerHash = hash(canonical(headerValue));
    immutable(header, headerValue);
    if (present(path.join(operation.directory, 'fatal.json'))) {
      readPrivateJson(path.join(operation.directory, 'fatal.json'));
      throw new ControlInvariantError('Retained control contract failure requires review');
    }
    const receipt = path.join(operation.directory, 'receipt.json');
    if (present(receipt)) return this.readReceipt(operation);
    if (present(path.join(operation.directory, 'intent.json'))) return this.confirm(operation, true);
    if (fs.readdirSync(operation.directory).some(name => name !== 'operation.json')) {
      throw new ControlInvariantError('Saved signed intent is missing from retained operation state');
    }
    this.active = operation;
    let failure;
    try { await action(); } catch (error) {
      if (error instanceof ControlInvariantError) {
        immutable(path.join(operation.directory, 'fatal.json'), { binding: this.binding, reason: 'single-transaction contract changed' });
        throw error;
      }
      failure = error;
    } finally { this.active = undefined; }
    if (!present(path.join(operation.directory, 'intent.json'))) throw failure ?? new Error('Operation produced no saved signed transaction');
    return this.confirm(operation, Boolean(failure));
  }
}
