//! The existing v0 token withdrawal transcript and Portal consume-instruction wire image.
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PAYLOAD_LEN: usize = 273;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const AUTHORIZATION_DOMAIN: [u8; 43] = *b"northstar-token-withdrawal-authorization-v0";
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EFFECT_DOMAIN: [u8; 16] = *b"token_withdrawal";

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct Payout<'a> {
    pub bridge_program: &'a [u8; 32],
    pub session_bridge: &'a [u8; 32],
    pub er_token_account: &'a [u8; 32],
    pub vault: &'a [u8; 32],
    pub vault_token_account: &'a [u8; 32],
    pub destination_token_account: &'a [u8; 32],
    pub mint: &'a [u8; 32],
    pub token_program: &'a [u8; 32],
    pub amount: u64,
    pub withdrawn: u64,
    pub decimals: u8,
}

// Verus does not specify u64::to_le_bytes at the pinned revision. Keep the actual
// no_std implementation visible to the verifier rather than assuming its contract.
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r@ =~= crate::wire_spec::le(x)))]
fn little_endian(x: u64) -> [u8; 8] {
    [
        x as u8,
        (x >> 8) as u8,
        (x >> 16) as u8,
        (x >> 24) as u8,
        (x >> 32) as u8,
        (x >> 40) as u8,
        (x >> 48) as u8,
        (x >> 56) as u8,
    ]
}

impl Payout<'_> {
    #[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::wire_spec::layout(self, r@)))]
    pub fn payload(&self) -> [u8; PAYLOAD_LEN] {
        let mut out = [0; PAYLOAD_LEN];
        out[..32].copy_from_slice(self.bridge_program);
        out[32..64].copy_from_slice(self.session_bridge);
        out[64..96].copy_from_slice(self.er_token_account);
        out[96..128].copy_from_slice(self.vault);
        out[128..160].copy_from_slice(self.vault_token_account);
        out[160..192].copy_from_slice(self.destination_token_account);
        out[192..224].copy_from_slice(self.mint);
        out[224..256].copy_from_slice(self.token_program);
        out[256..264].copy_from_slice(&little_endian(self.amount));
        out[264..272].copy_from_slice(&little_endian(self.withdrawn));
        out[272] = self.decimals;
        out
    }
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::wire_spec::auth_parts(data,r)))]
pub(crate) fn authorization_parts(data: &[u8; PAYLOAD_LEN]) -> [&[u8]; 2] {
    [&AUTHORIZATION_DOMAIN, data]
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::wire_spec::effect_parts(previous,data,r)))]
pub(crate) fn settlement_parts<'a>(
    previous: &'a [u8; 32],
    data: &'a [u8; PAYLOAD_LEN],
) -> [&'a [u8]; 3] {
    [previous, &EFFECT_DOMAIN, data]
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures crate::wire_spec::consume(slot,checksum,amount,withdrawn,decimals,r@)))]
pub fn consume_instruction(
    slot: u64,
    checksum: &[u8; 32],
    amount: u64,
    withdrawn: u64,
    decimals: u8,
) -> [u8; 58] {
    let mut data = [0; 58];
    data[0] = 27;
    data[1..9].copy_from_slice(&little_endian(slot));
    data[9..41].copy_from_slice(checksum);
    data[41..49].copy_from_slice(&little_endian(amount));
    data[49..57].copy_from_slice(&little_endian(withdrawn));
    data[57] = decimals;
    data
}
