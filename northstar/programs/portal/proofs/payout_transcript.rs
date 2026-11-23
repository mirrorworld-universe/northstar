#![allow(dead_code)]
#[path = "../src/payout_wire.rs"]
pub(crate) mod payout_wire;

pub mod wire_spec {
    use super::payout_wire::*;
    use vstd::prelude::*;

    verus! {
    pub open spec fn le(x: u64) -> Seq<u8> {
        seq![x as u8, (x >> 8) as u8, (x >> 16) as u8, (x >> 24) as u8,
             (x >> 32) as u8, (x >> 40) as u8, (x >> 48) as u8, (x >> 56) as u8]
    }

    spec fn decode(s: Seq<u8>) -> u64 {
        s[0] as u64 | (s[1] as u64) << 8 | (s[2] as u64) << 16 | (s[3] as u64) << 24 |
        (s[4] as u64) << 32 | (s[5] as u64) << 40 | (s[6] as u64) << 48 | (s[7] as u64) << 56
    }

    proof fn le_roundtrip(x: u64) ensures decode(le(x)) == x {
        assert(((x as u8 as u64) | ((x >> 8) as u8 as u64) << 8 | ((x >> 16) as u8 as u64) << 16 |
            ((x >> 24) as u8 as u64) << 24 | ((x >> 32) as u8 as u64) << 32 |
            ((x >> 40) as u8 as u64) << 40 | ((x >> 48) as u8 as u64) << 48 |
            ((x >> 56) as u8 as u64) << 56) == x) by (bit_vector);
    }

    pub open spec fn layout(w: &Payout, s: Seq<u8>) -> bool {
        &&& s.len() == 273
        &&& s.subrange(0, 32) =~= w.bridge_program@
        &&& s.subrange(32, 64) =~= w.session_bridge@
        &&& s.subrange(64, 96) =~= w.er_token_account@
        &&& s.subrange(96, 128) =~= w.vault@
        &&& s.subrange(128, 160) =~= w.vault_token_account@
        &&& s.subrange(160, 192) =~= w.destination_token_account@
        &&& s.subrange(192, 224) =~= w.mint@
        &&& s.subrange(224, 256) =~= w.token_program@
        &&& s.subrange(256, 264) =~= le(w.amount)
        &&& s.subrange(264, 272) =~= le(w.withdrawn)
        &&& s[272] == w.decimals
    }

    pub(crate) open spec fn same(a: &Payout, b: &Payout) -> bool {
        &&& a.bridge_program@ =~= b.bridge_program@
        &&& a.session_bridge@ =~= b.session_bridge@
        &&& a.er_token_account@ =~= b.er_token_account@
        &&& a.vault@ =~= b.vault@
        &&& a.vault_token_account@ =~= b.vault_token_account@
        &&& a.destination_token_account@ =~= b.destination_token_account@
        &&& a.mint@ =~= b.mint@
        &&& a.token_program@ =~= b.token_program@
        &&& a.amount == b.amount && a.withdrawn == b.withdrawn && a.decimals == b.decimals
    }

    // This is injectivity of bytes before SHA-256, not injectivity of the digest.
    pub(crate) proof fn payload_injective(a: &Payout, b: &Payout, bytes: Seq<u8>)
        requires layout(a, bytes), layout(b, bytes),
        ensures same(a, b),
    {
        le_roundtrip(a.amount);
        le_roundtrip(b.amount);
        le_roundtrip(a.withdrawn);
        le_roundtrip(b.withdrawn);
    }

    pub open spec fn auth_parts(s: &[u8; 273], parts: [&[u8]; 2]) -> bool {
        (parts[0]@ =~= AUTHORIZATION_DOMAIN@) && (parts[1]@ =~= s@)
    }

    pub open spec fn effect_parts(prev: &[u8; 32], s: &[u8; 273], parts: [&[u8]; 3]) -> bool {
        (parts[0]@ =~= prev@) && (parts[1]@ =~= EFFECT_DOMAIN@) && (parts[2]@ =~= s@)
    }

    fn separated_messages(a: &Payout, b: &Payout, prev: &[u8; 32]) {
        let x = a.payload();
        let y = b.payload();
        let auth = authorization_parts(&x);
        let effect = settlement_parts(prev, &y);
        let ghost p = auth[0]@ + auth[1]@;
        let ghost q = effect[0]@ + effect[1]@ + effect[2]@;
        assert(p.len() == 316 && q.len() == 321 && p != q);
    }

    pub open spec fn consume(slot: u64, checksum: &[u8; 32], amount: u64, target: u64,
        decimals: u8, s: Seq<u8>) -> bool {
        &&& s.len() == 58 && s[0] == 27 && s[57] == decimals
        &&& s.subrange(1, 9) =~= le(slot)
        &&& s.subrange(9, 41) =~= checksum@
        &&& s.subrange(41, 49) =~= le(amount)
        &&& s.subrange(49, 57) =~= le(target)
    }

    fn cpi_roundtrip(slot: u64, checksum: &[u8; 32], amount: u64, target: u64, decimals: u8) {
        let call = consume_instruction(slot, checksum, amount, target, decimals);
        proof { le_roundtrip(slot); le_roundtrip(amount); le_roundtrip(target); }
        assert(decode(call@.subrange(1, 9)) == slot);
        assert(decode(call@.subrange(41, 49)) == amount);
        assert(decode(call@.subrange(49, 57)) == target);
        assert(call@.subrange(9, 41) =~= checksum@);
        assert(call@[0] == 27 && call@[57] == decimals);
    }

    fn reachable_domains(key: &[u8; 32]) {
        let w = Payout {
            bridge_program: key, session_bridge: key, er_token_account: key, vault: key,
            vault_token_account: key, destination_token_account: key, mint: key, token_program: key,
            amount: 256, withdrawn: u64::MAX, decimals: 9,
        };
        let data = w.payload();
        assert(256u64 as u8 == 0 && (256u64 >> 8) as u8 == 1 && (u64::MAX >> 56) as u8 == 255) by (bit_vector);
        assert(le(256)[0] == 0 && le(256)[1] == 1 && le(u64::MAX)[7] == 255);
        assert(data@.subrange(256, 264)[0] == data@[256]);
        assert(data@.subrange(256, 264)[1] == data@[257]);
        assert(data@.subrange(264, 272)[7] == data@[271]);
        assert(data@[256] == 0 && data@[257] == 1 && data@[271] == 255 && data@[272] == 9);
    }
    }
}
