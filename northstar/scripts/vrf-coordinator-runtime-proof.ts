// Local test-grade reference proofs and bounded input selection; never execution-validity proofs.
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

const source = process.env.VRF_REFERENCE_SOURCE;
if (!source) throw new Error('VRF_REFERENCE_SOURCE must name the pinned library source');
const mode = process.argv[2];
if (mode !== '--search-seed' && !/^[0-9a-f]{64}$/.test(mode ?? '')) throw new Error('Expected a 32-byte alpha in hex');
const { prove, verify, vrfPublicKey, encodeToCurve } = await import(pathToFileURL(resolve(source,
  'sdks/packages/vrf-services/src/ecvrf.ts')).href);
// Public RFC 9381 Example 16 test secret. Never use this key outside local tests.
const secret = Buffer.from('9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60', 'hex');
const key = vrfPublicKey(secret);
if (mode === '--search-seed') {
  const input = JSON.parse(process.argv[3] ?? '{}');
  const bytes = (name: string) => {
    const value = input[name];
    if (!Array.isArray(value) || value.length !== 32 || !value.every(n => Number.isInteger(n) && n >= 0 && n <= 255)) throw new Error(`Expected 32 bytes: ${name}`);
    return Buffer.from(value);
  };
  const u64 = (name: string) => {
    if (!Number.isSafeInteger(input[name]) || input[name] < 0) throw new Error(`Expected safe slot: ${name}`);
    const out = Buffer.alloc(8); out.writeBigUInt64LE(BigInt(input[name])); return out;
  };
  const minimum = input.minimum_tries;
  if (!Number.isInteger(minimum) || minimum < 1 || minimum > 20) throw new Error('minimum_tries must be 1..20');
  const coordinator = bytes('coordinator'), request = bytes('request'), hash = bytes('slot_hash');
  const slot = u64('request_slot'), hashSlot = u64('hash_slot');
  for (let counter = 0; counter < 100_000; counter++) {
    const seed = Buffer.alloc(32, 0x19); seed.writeUInt32LE(counter);
    const alpha = createHash('sha256').update(Buffer.concat([Buffer.from('sonic-vrf/v1'), coordinator, request, seed, slot, hashSlot, hash])).digest();
    const { tries } = encodeToCurve(key, alpha);
    if (tries >= minimum) {
      console.log(JSON.stringify({ seed: [...seed], predicted_alpha: [...alpha], tries, candidates: counter + 1 }));
      process.exit(0);
    }
  }
  throw new Error('Bounded seed search exhausted');
}
const alpha = Buffer.from(mode, 'hex');
const { proof, output, tries } = prove(secret, alpha);
if (!Buffer.from(verify(key, alpha, proof)).equals(Buffer.from(output))) throw new Error('Reference self-check failed');
console.log(JSON.stringify({ key: [...key], proof: [...proof], output: [...output], tries }));
