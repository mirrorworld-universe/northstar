//! Diagnostic SHA-256 known-answer tests. These are not application fixtures,
//! witness outputs, or a replacement for real SBPF execution/binding.
use sha2::{Digest, Sha256, Sha256VarCore, compress256, digest::core_api::Block};

const PATTERNS: &[(usize, &str)] = &[
    (
        0,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ),
    (
        1,
        "ca358758f6d27e6cf45272937977a748fd88391db679ceda7dc7bf1f005ee879",
    ),
    (
        3,
        "647674a296197442f518bcca323ec605dd8d098b2d4f22ee1fdcdd2bb753a189",
    ),
    (
        55,
        "8aa994584139d128848eeebc4e815639ba5ab6e6e39574195a63ac4f14f7c43b",
    ),
    (
        56,
        "ad574708f75c044c9b85de64cb568ee7711ff4f36448c6242f053ba8f6cc2b63",
    ),
    (
        63,
        "280ed3e8ff1df845b2e7dfe6ac6cee817bef20e783cc65abc41b818b4d2fe076",
    ),
    (
        64,
        "c6ab9724ade5b6a7a1edfffb12f3aa9181351355af8fd08c919952ad211339dd",
    ),
    (
        65,
        "788367c73c7ddf4c53f65e68cc0d943e6227ab55b0e78ba63ace822b1c6301c0",
    ),
    (
        127,
        "192409cd280e14b743642ad1343fbd3e82d9305de72c078117745a679210cc3d",
    ),
    (
        128,
        "cc548ca2dec1f6fe4f58b2e27aa9c7521607df1130d140b55a4dad0665302356",
    ),
    (
        129,
        "81e89a7b2911aaa7795f9e3d4910cb47d6cd2b00d83b8399481527261a1a7519",
    ),
    (
        4096,
        "d41d438c379110c7f7b2c561b1f04f26c1b4549110791f8e022f48974280c13e",
    ),
    (
        65536,
        "ef4636928161808e87035fa51983821677527ccd9661991c5d0126a778b2268a",
    ),
];
const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const MILLION_A: &str = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";
const IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

fn expected(hex: &str) -> [u8; 32] {
    let mut result = [0; 32];
    assert_eq!(hex.len(), 64);
    for (pair, byte) in hex.as_bytes().as_chunks::<2>().0.iter().zip(&mut result) {
        *byte = u8::from_str_radix(core::str::from_utf8(pair).expect("constant ASCII"), 16)
            .expect("constant hex");
    }
    result
}
fn pattern(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(7))
        .collect()
}
fn hash(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}
fn hash_variants(data: &[u8], answer: &[u8; 32]) {
    assert_eq!(&hash(data), answer);
    for chunk in [1, 7, 63, 64, 65, 127] {
        let mut digest = Sha256::new();
        for part in data.chunks(chunk) {
            digest.update(part);
        }
        let computed: [u8; 32] = digest.finalize().into();
        assert_eq!(&computed, answer);
    }
    for offset in 0..8usize {
        let mut storage = vec![0; data.len().saturating_add(8)];
        storage[offset..offset.saturating_add(data.len())].copy_from_slice(data);
        assert_eq!(
            &hash(&storage[offset..offset.saturating_add(data.len())]),
            answer
        );
    }
}

/// Covers padding boundaries, incremental updates, unaligned buffers, full-block
/// compression at every alignment, and no-op compression. Guest diagnostics must
/// still execute a real supplied candidate envelope after these checks.
pub fn check() {
    hash_variants(b"abc", &expected(ABC));
    for &(length, answer) in PATTERNS {
        hash_variants(&pattern(length), &expected(answer));
    }
    let data = vec![b'a'; 1_000_000];
    assert_eq!(hash(&data), expected(MILLION_A));
    let mut stream = Sha256::new();
    for part in data.chunks(63) {
        stream.update(part);
    }
    let streamed: [u8; 32] = stream.finalize().into();
    assert_eq!(streamed, expected(MILLION_A));
    for offset in 0..8usize {
        let mut storage = [0u8; 72];
        let end = offset.saturating_add(64);
        let block = &mut storage[offset..end];
        block[..3].copy_from_slice(b"abc");
        block[3] = 0x80;
        block[63] = 24;
        let block: &Block<Sha256VarCore> = (&*block).into();
        let mut state = IV;
        compress256(&mut state, core::slice::from_ref(block));
        let mut result = [0u8; 32];
        for (word, output) in state.iter().zip(result.as_chunks_mut::<4>().0.iter_mut()) {
            output.copy_from_slice(&word.to_be_bytes());
        }
        assert_eq!(result, expected(ABC));
    }
    let mut unchanged = [0x12345678; 8];
    compress256(&mut unchanged, &[]);
    assert_eq!(unchanged, [0x12345678; 8]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invariant_sha256_known_answers_and_alignment() {
        check();
    }

    #[test]
    fn invariant_sha256_matches_independent_native_version() {
        use sha2_reference::Digest as _;
        for &(length, _) in PATTERNS {
            let data = pattern(length);
            let reference: [u8; 32] = sha2_reference::Sha256::digest(&data).into();
            assert_eq!(hash(&data), reference);
        }
    }

    #[test]
    fn invariant_compiled_policy_digest_is_unchanged() {
        let profile = northstar_vrf_sbpf_replay::development_profile();
        let digest =
            northstar_transaction_proof::vrf_candidate::candidate_profile_sha256(&profile).unwrap();
        assert_eq!(
            digest,
            expected("c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d")
        );
    }
}
