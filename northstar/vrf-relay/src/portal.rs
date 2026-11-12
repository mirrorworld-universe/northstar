//! Native continuity-session wire checks, not L1 or execution authentication.
//! The layout and commitment match Northstar's `SessionV2` and `SessionIdentityV2`.
//! [Schema reference](https://github.com/mirrorworld-universe/northstar/blob/7b8fea2c41a6af63cd25152e43338553c9be204d/northstar/programs/portal/src/continuity.rs).
use {
    crate::rpc::Rpc,
    anyhow::{Context, Result, ensure},
    serde_json::json,
    sha2::{Digest, Sha256},
    solana_pubkey::Pubkey,
};
pub const SESSION_BYTES: usize = 378;
const SESSION_TAG: &[u8; 8] = b"NSSESS02";
const BASE_SESSION_TAG: [u8; 8] = [243, 81, 72, 115, 214, 188, 72, 144];
pub fn parse_context(value: &str) -> Result<[u8; 32]> {
    ensure!(value.len() == 64, "session context must be 32-byte hex");
    let context = hex::decode(value)?
        .try_into()
        .ok()
        .context("session context must be 32 bytes")?;
    ensure!(context != [0; 32], "session context must be nonzero");
    Ok(context)
}
pub fn session_address(portal: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"session"], portal)
}
pub fn deposit_receipt_address(
    portal: &Pubkey,
    context: &[u8; 32],
    recipient: &Pubkey,
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"deposit_receipt_v2", context, recipient.as_ref()],
        portal,
    )
}
pub fn ingress_record_address(portal: &Pubkey, context: &[u8; 32], sequence: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"ingress_v2", context, &sequence.to_le_bytes()], portal)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionIdentity {
    pub l1_genesis: [u8; 32],
    pub portal: Pubkey,
    pub session: Pubkey,
    pub nonce: u128,
    pub grid_id: u64,
    pub validator: Pubkey,
    pub profile: [u8; 32],
}
impl SessionIdentity {
    pub fn commitment(&self) -> Result<[u8; 32]> {
        ensure!(
            self.nonce != 0
                && ![
                    self.l1_genesis,
                    self.portal.to_bytes(),
                    self.session.to_bytes(),
                    self.validator.to_bytes(),
                    self.profile,
                ]
                .contains(&[0; 32]),
            "continuity session identity is incomplete"
        );
        let mut encoded = Vec::with_capacity(185);
        encoded.push(2);
        encoded.extend_from_slice(&self.l1_genesis);
        encoded.extend_from_slice(self.portal.as_ref());
        encoded.extend_from_slice(self.session.as_ref());
        encoded.extend_from_slice(&self.nonce.to_le_bytes());
        encoded.extend_from_slice(&self.grid_id.to_le_bytes());
        encoded.extend_from_slice(self.validator.as_ref());
        encoded.extend_from_slice(&self.profile);
        let mut hash = Sha256::new();
        hash.update(b"northstar-continuity-v2");
        hash.update(7u16.to_le_bytes());
        hash.update(b"session");
        hash.update(185u16.to_le_bytes());
        hash.update(&encoded);
        let mut root: [u8; 32] = hash.finalize().into();
        root[0] &= 0x1f;
        Ok(root)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub grid_id: u64,
    pub ttl_slots: u64,
    pub fee_cap: u64,
    pub created_at: u64,
    pub nonce: u128,
    pub authority: Pubkey,
    pub validator: Pubkey,
    pub settlement_interval_slots: u64,
    pub last_settled_l1_slot: u64,
    pub last_settled_er_slot: u64,
    pub settlement_status: u8,
    pub bump: u8,
    pub l1_genesis: [u8; 32],
    pub profile: [u8; 32],
    pub ingress_count: u64,
    pub ingress_root: [u8; 32],
    pub consumed_ingress_count: u64,
    pub consumed_ingress_root: [u8; 32],
}
fn field<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    Ok(data
        .get(offset..offset + N)
        .context("truncated session field")?
        .try_into()?)
}
impl Session {
    pub fn decode(data: &[u8]) -> Result<Self> {
        ensure!(
            data.len() == SESSION_BYTES,
            "unsupported Portal session length"
        );
        ensure!(
            data.get(..8) == Some(SESSION_TAG.as_slice()),
            "unsupported Portal session frame"
        );
        ensure!(
            field::<8>(data, 8)? == BASE_SESSION_TAG,
            "invalid embedded session discriminator"
        );
        let result = Self {
            grid_id: u64::from_le_bytes(field(data, 16)?),
            ttl_slots: u64::from_le_bytes(field(data, 24)?),
            fee_cap: u64::from_le_bytes(field(data, 32)?),
            created_at: u64::from_le_bytes(field(data, 40)?),
            nonce: u128::from_le_bytes(field(data, 48)?),
            authority: Pubkey::new_from_array(field(data, 64)?),
            validator: Pubkey::new_from_array(field(data, 96)?),
            settlement_interval_slots: u64::from_le_bytes(field(data, 128)?),
            last_settled_l1_slot: u64::from_le_bytes(field(data, 136)?),
            last_settled_er_slot: u64::from_le_bytes(field(data, 144)?),
            settlement_status: data[152],
            bump: data[233],
            l1_genesis: field(data, 234)?,
            profile: field(data, 266)?,
            ingress_count: u64::from_le_bytes(field(data, 298)?),
            ingress_root: field(data, 306)?,
            consumed_ingress_count: u64::from_le_bytes(field(data, 338)?),
            consumed_ingress_root: field(data, 346)?,
        };
        ensure!(
            result.nonce != 0
                && result.l1_genesis != [0; 32]
                && result.profile != [0; 32]
                && result.validator != Pubkey::default()
                && result.settlement_status <= 1
                && result.consumed_ingress_count <= result.ingress_count
                && (result.ingress_count == 0) == (result.ingress_root == [0; 32])
                && (result.consumed_ingress_count == 0)
                    == (result.consumed_ingress_root == [0; 32]),
            "invalid continuity session state"
        );
        Ok(result)
    }
    pub fn identity(&self, portal: Pubkey, session: Pubkey) -> SessionIdentity {
        SessionIdentity {
            l1_genesis: self.l1_genesis,
            portal,
            session,
            nonce: self.nonce,
            grid_id: self.grid_id,
            validator: self.validator,
            profile: self.profile,
        }
    }
    pub fn check_account(
        &self,
        portal: Pubkey,
        session: Pubkey,
        owner: Pubkey,
        executable: bool,
    ) -> Result<()> {
        let (expected, bump) = session_address(&portal);
        ensure!(
            portal != Pubkey::default() && owner == portal && !executable,
            "Portal session account owner or executable binding differs"
        );
        ensure!(
            session == expected && self.bump == bump,
            "Portal session PDA binding differs"
        );
        Ok(())
    }
    pub fn check_l1_slot(&self, current_l1_slot: u64) -> Result<()> {
        ensure!(
            current_l1_slot >= self.created_at
                && current_l1_slot <= self.created_at.saturating_add(self.ttl_slots),
            "Portal session is not live at the observed L1 slot"
        );
        Ok(())
    }
}
pub fn check_rpc_binding(
    l1: &Rpc,
    er: &Rpc,
    expected: Pubkey,
    pinned: [u8; 32],
    genesis: &str,
) -> Result<(Session, u64)> {
    let observed = er.call("getSessionPda", json!([]))?;
    ensure!(
        crate::rpc::string(&observed)?.parse::<Pubkey>()? == expected,
        "ER session changed"
    );
    let genesis_bytes: [u8; 32] = bs58::decode(genesis)
        .into_vec()
        .context("invalid L1 genesis encoding")?
        .try_into()
        .ok()
        .context("L1 genesis must be a complete canonical hash")?;
    ensure!(
        bs58::encode(genesis_bytes).into_string() == genesis,
        "L1 genesis is noncanonical"
    );
    let (l1_context, account) = l1.account_at(&expected, l1.slot()?)?;
    let l1_account = account.context("L1 continuity session missing")?;
    let er_account = er
        .account(&expected, er.slot()?)?
        .context("ER continuity session missing")?;
    let portal = l1_account.owner;
    let l1_frame = Session::decode(&l1_account.data)?;
    let er_frame = Session::decode(&er_account.data)?;
    for (frame, account) in [(&l1_frame, &l1_account), (&er_frame, &er_account)] {
        frame.check_account(portal, expected, account.owner, account.executable)?;
        ensure!(
            frame.created_at <= l1_context,
            "session creation is newer than its L1 context"
        );
        ensure!(
            frame.l1_genesis == genesis_bytes,
            "session L1 genesis binding differs"
        );
        ensure!(
            frame.identity(portal, expected).commitment()? == pinned,
            "continuity session context changed"
        );
    }
    Ok((l1_frame, l1_context))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn frame() -> [u8; SESSION_BYTES] {
        let mut data = [0; SESSION_BYTES];
        data[..8].copy_from_slice(SESSION_TAG);
        data[8..16].copy_from_slice(&BASE_SESSION_TAG);
        for (offset, value) in [
            (16, 1u64),
            (24, 2_000),
            (32, 1_000),
            (40, 10),
            (128, 10),
            (298, 2),
            (338, 1),
        ] {
            data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        data[48..64].copy_from_slice(&7u128.to_le_bytes());
        data[64..96].fill(1);
        data[96..128].fill(2);
        data[234..266].fill(3);
        data[266..298].fill(4);
        data[306..338].fill(5);
        data[346..378].fill(6);
        data[233] = Pubkey::find_program_address(&[b"session"], &Pubkey::new_from_array([9; 32])).1;
        data
    }
    #[test]
    fn addresses_match_independent_native_portal_vectors() {
        let portal = Pubkey::new_from_array([9; 32]);
        let recipient = Pubkey::new_from_array([3; 32]);
        let (session, bump) = session_address(&portal);
        assert_eq!(
            session.to_string(),
            "HQWGqRo2LeDMGNesadaohS7o9u8PtTcz4igFkcJ6WwTz"
        );
        assert_eq!(bump, 250);
        let (receipt, bump) = deposit_receipt_address(&portal, &[1; 32], &recipient);
        assert_eq!(
            receipt.to_string(),
            "EZUJpehaV5Mjf47vatSwWw9puQtCuB6GJvw5urrXiEdK"
        );
        assert_eq!(bump, 255);
        for (sequence, expected, bump) in [
            (7, "FHf649UHZ8jQrJpyR4Lpwdz42RvmHLhCjhvVa5Fn4Pnc", 255),
            (8, "FgPz2G5F7hj8JqzAunmCDzfC3KJ1kLsAqzYwwftx1YCo", 253),
        ] {
            let actual = ingress_record_address(&portal, &[1; 32], sequence);
            assert_eq!(actual.0.to_string(), expected);
            assert_eq!(actual.1, bump);
        }
        assert_ne!(
            receipt,
            deposit_receipt_address(&portal, &[2; 32], &recipient).0
        );
    }
    #[test]
    fn context_hex_is_exact_nonzero_and_case_independent() {
        assert_eq!(parse_context(&"AF".repeat(32)).unwrap(), [0xaf; 32]);
        assert_eq!(parse_context(&"01".repeat(32)).unwrap(), [1; 32]);
        for value in [
            String::new(),
            "00".repeat(32),
            "0g".repeat(32),
            "1".repeat(63),
            "1".repeat(65),
        ] {
            assert!(parse_context(&value).is_err());
        }
    }
    #[test]
    fn identity_matches_independent_native_low_and_high_nonce_vectors() {
        let session = Session::decode(&frame()).unwrap();
        for (nonce, expected) in [
            (
                7,
                "01beb461f321f75cb0a09858be79ea208f6ca7e2d4172cf78d871108369aa647",
            ),
            (
                (1u128 << 100) + 7,
                "0fc5636a0a9f37d69ed2b9adcac3337e045308d50b8b9118663349877616b4ce",
            ),
        ] {
            let mut identity = session.identity(
                Pubkey::new_from_array([9; 32]),
                Pubkey::new_from_array([8; 32]),
            );
            identity.nonce = nonce;
            assert_eq!(hex::encode(identity.commitment().unwrap()), expected);
            identity.nonce = 0;
            assert!(identity.commitment().is_err());
        }
    }
    #[test]
    fn immutable_identity_excludes_mutable_input_progress() {
        let mut data = frame();
        let before = Session::decode(&data).unwrap();
        data[338..346].copy_from_slice(&2u64.to_le_bytes());
        data[346..378].copy_from_slice(&[5; 32]);
        let after = Session::decode(&data).unwrap();
        let portal = Pubkey::new_from_array([9; 32]);
        let session = Pubkey::new_from_array([8; 32]);
        assert_eq!(
            before.identity(portal, session),
            after.identity(portal, session)
        );
        assert_eq!(after.consumed_ingress_count, 2);
    }
    #[test]
    fn only_exact_current_frames_and_valid_input_counters_are_accepted() {
        let data = frame();
        assert!(Session::decode(&data[8..234]).is_err());
        assert!(Session::decode(&[data.as_slice(), &[0]].concat()).is_err());
        for offset in [0, 8] {
            let mut changed = data;
            changed[offset] ^= 1;
            assert!(Session::decode(&changed).is_err());
        }
        for (start, end) in [
            (48, 64),
            (96, 128),
            (234, 266),
            (266, 298),
            (306, 338),
            (346, 378),
        ] {
            let mut changed = data;
            changed[start..end].fill(0);
            assert!(Session::decode(&changed).is_err());
        }
        let mut changed = data;
        changed[338..346].copy_from_slice(&3u64.to_le_bytes());
        assert!(Session::decode(&changed).is_err());
        changed = data;
        changed[152] = 2;
        assert!(Session::decode(&changed).is_err());
    }
    #[test]
    fn account_and_l1_expiry_checks_bind_the_canonical_session() {
        let portal = Pubkey::new_from_array([9; 32]);
        let address = Pubkey::find_program_address(&[b"session"], &portal).0;
        let session = Session::decode(&frame()).unwrap();
        session
            .check_account(portal, address, portal, false)
            .unwrap();
        assert!(
            session
                .check_account(portal, address, Pubkey::new_unique(), false)
                .is_err()
        );
        assert!(
            session
                .check_account(portal, Pubkey::new_unique(), portal, false)
                .is_err()
        );
        assert!(
            session
                .check_account(portal, address, portal, true)
                .is_err()
        );
        assert!(session.check_l1_slot(9).is_err());
        session.check_l1_slot(10).unwrap();
        session.check_l1_slot(2_010).unwrap();
        assert!(session.check_l1_slot(2_011).is_err());
    }
}
