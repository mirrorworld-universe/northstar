//! Arkworks 0.6 still requires rand 0.8 traits; preserve the ChaCha20 byte stream.
use ark_std::rand::{CryptoRng, Error, RngCore, SeedableRng};

pub struct ChaCha20Rng(rand_chacha::ChaCha20Rng);

impl SeedableRng for ChaCha20Rng {
    type Seed = [u8; 32];

    fn from_seed(seed: Self::Seed) -> Self {
        Self(rand_chacha::rand_core::SeedableRng::from_seed(seed))
    }
}

impl RngCore for ChaCha20Rng {
    fn next_u32(&mut self) -> u32 {
        rand_chacha::rand_core::Rng::next_u32(&mut self.0)
    }

    fn next_u64(&mut self) -> u64 {
        rand_chacha::rand_core::Rng::next_u64(&mut self.0)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rand_chacha::rand_core::Rng::fill_bytes(&mut self.0, dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for ChaCha20Rng {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chacha20_zero_seed_matches_reference_stream() {
        let mut rng = ChaCha20Rng::from_seed([0; 32]);
        let mut bytes = [0; 16];
        rng.fill_bytes(&mut bytes);
        assert_eq!(hex::encode(bytes), "76b8e0ada0f13d90405d6ae55386bd28");
    }
}
