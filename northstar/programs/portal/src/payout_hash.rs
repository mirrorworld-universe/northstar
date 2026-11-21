use {
    crate::payout_wire::{authorization_parts, settlement_parts, Payout},
    solana_sha256_hasher::hashv,
};

impl Payout<'_> {
    pub fn authorization_hash(&self) -> [u8; 32] {
        hashv(&authorization_parts(&self.payload())).to_bytes()
    }

    pub fn accumulate(&self, previous: &[u8; 32]) -> [u8; 32] {
        hashv(&settlement_parts(previous, &self.payload())).to_bytes()
    }
}
