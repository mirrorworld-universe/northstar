use northstar_portal::{
    payout_wire::{consume_instruction, Payout},
    ConsumeTokenWithdrawal, PortalInstruction,
};

#[test]
fn payout_hashes_match_independent_v0_golden_vectors() {
    // Fixture: bytes 00..ff in eight distinct positions, two non-palindromic u64s,
    // decimal byte 09 and accumulator a5 repeated. Digests computed with Python hashlib.
    let keys: [[u8; 32]; 8] =
        core::array::from_fn(|k| core::array::from_fn(|i| (32 * k + i) as u8));
    let w = Payout {
        bridge_program: &keys[0],
        session_bridge: &keys[1],
        er_token_account: &keys[2],
        vault: &keys[3],
        vault_token_account: &keys[4],
        destination_token_account: &keys[5],
        mint: &keys[6],
        token_program: &keys[7],
        amount: 0x0102_0304_0506_0708,
        withdrawn: 0xf1e2_d3c4_b5a6_9788,
        decimals: 9,
    };
    assert_eq!(
        hex::encode(w.authorization_hash()),
        "b78f0a8c6c718c8e50b2504af6909fa6a59b5c2d6f61a4434f25875a7acd1282"
    );
    assert_eq!(
        hex::encode(w.accumulate(&[0xa5; 32])),
        "1226e4e6d41f57d64c8e33291919ad978bda666f8511e96aa655cc1ff1e75556"
    );
}

#[test]
fn bridge_cpi_matches_portal_borsh_encoding() {
    for n in [0, 1, 255, 256, 0x0123_4567_89ab_cdef, u64::MAX] {
        let slot = n.rotate_left(17);
        let target = n.reverse_bits();
        let checksum = core::array::from_fn(|i| i as u8);
        let decimals = n as u8;
        let ix = PortalInstruction::ConsumeTokenWithdrawal(ConsumeTokenWithdrawal {
            er_slot: slot,
            checksum,
            amount: n,
            withdrawn: target,
            decimals,
        });
        let wire = consume_instruction(slot, &checksum, n, target, decimals);
        assert_eq!(borsh::to_vec(&ix).unwrap(), wire);
        let PortalInstruction::ConsumeTokenWithdrawal(decoded) = borsh::from_slice(&wire).unwrap()
        else {
            panic!("wrong instruction tag")
        };
        assert_eq!(
            (
                decoded.er_slot,
                decoded.checksum,
                decoded.amount,
                decoded.withdrawn,
                decoded.decimals
            ),
            (slot, checksum, n, target, decimals)
        );
    }
}
