//! Integration tests for `Portal::Delegate`'s two flows:
//!
//! - **Flow 1 (keypair wallet)**: 5 accounts. Pre-existing behavior used by NorthStarSDK
//!   for "fast trading wallet" delegation. Tested for backwards compatibility.
//! - **Flow 2 (PDA with data)**: 6 accounts (adds a buffer at index 5). New flow for
//!   stateful program-owned PDAs (AMM pools, vaults, etc.). Tests cover the happy path
//!   and the four rejection cases (wrong buffer PDA, wrong buffer owner, wrong size,
//!   missing delegated-account signer).
//!
//! The buffer dance that the *caller program* performs (allocate buffer, copy data,
//! zero original, reassign owner) is not Portal's responsibility and is not exercised
//! here. We instead pre-stage the post-dance state via `ProgramTest::add_account` so
//! the tests focus on Portal's new validation + data-copy logic.

#![cfg(test)]

use {
    borsh::BorshDeserialize,
    northstar_portal::{DelegationRecord, PortalInstruction},
    solana_account::Account,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_program_test::{BanksClient, ProgramTest, ProgramTestContext},
    solana_pubkey::Pubkey,
    solana_signer::Signer,
    solana_system_interface::program as system_program,
    solana_transaction::Transaction,
};

const PORTAL_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("GikCSCpYUq7QR7esoK6GM4UbJzKgdKNvS5bR1rBYH5E4");

fn find_delegation_record_pda(delegated_account: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"delegation", delegated_account.as_ref()],
        &PORTAL_PROGRAM_ID,
    )
}

fn find_delegate_buffer_pda(owner_program: &Pubkey, delegated_account: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[b"portal_buffer", delegated_account.as_ref()],
        owner_program,
    )
}

fn build_delegate_ix(
    payer: &Pubkey,
    delegated_account: &Pubkey,
    owner_program: &Pubkey,
    delegation_record: &Pubkey,
    grid_id: u64,
    buffer: Option<&Pubkey>,
) -> Instruction {
    let ix = PortalInstruction::Delegate { grid_id };
    let data = borsh::to_vec(&ix).unwrap();

    let mut accounts = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(*delegated_account, true),
        AccountMeta::new_readonly(*owner_program, false),
        AccountMeta::new(*delegation_record, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    if let Some(buf) = buffer {
        accounts.push(AccountMeta::new_readonly(*buf, false));
    }

    Instruction {
        program_id: PORTAL_PROGRAM_ID,
        accounts,
        data,
    }
}

/// Build a ProgramTest with Portal loaded plus a pre-staged delegated_account already
/// owned by Portal with the given data. Optionally pre-stage a buffer too.
struct DelegateScenario {
    payer: Keypair,
    delegated: Keypair,
    owner_program: Pubkey,
    grid_id: u64,
}

impl DelegateScenario {
    fn new() -> Self {
        Self {
            payer: Keypair::new(),
            delegated: Keypair::new(),
            owner_program: Pubkey::new_unique(),
            grid_id: 1,
        }
    }
}

struct StagedScenario {
    inner: DelegateScenario,
    program_test: ProgramTest,
    delegated_data: Vec<u8>,
    buffer_account: Option<(Pubkey, Vec<u8>, Pubkey)>, // (pubkey, data, owner)
}

impl StagedScenario {
    fn new(inner: DelegateScenario) -> Self {
        let mut program_test = ProgramTest::default();
        program_test.add_program("northstar_portal", PORTAL_PROGRAM_ID, None);
        Self {
            inner,
            program_test,
            delegated_data: vec![],
            buffer_account: None,
        }
    }

    fn with_delegated(mut self, data: Vec<u8>, owner: Pubkey) -> Self {
        let lamports = 10_000_000;
        self.delegated_data = data.clone();
        self.program_test.add_account(
            self.inner.delegated.pubkey(),
            Account {
                lamports,
                data,
                owner,
                executable: false,
                rent_epoch: 0,
            },
        );
        self
    }

    fn with_buffer(self, data: Vec<u8>) -> Self {
        let owner = self.inner.owner_program;
        let buffer_key = find_delegate_buffer_pda(&owner, &self.inner.delegated.pubkey()).0;
        self.add_buffer_inner(buffer_key, data, owner)
    }

    fn with_buffer_at_wrong_pda(self, data: Vec<u8>) -> Self {
        let buffer_key = Pubkey::new_unique(); // not the expected derivation
        let owner = self.inner.owner_program;
        self.add_buffer_inner(buffer_key, data, owner)
    }

    fn with_buffer_wrong_owner(self, data: Vec<u8>, wrong_owner: Pubkey) -> Self {
        let buffer_key =
            find_delegate_buffer_pda(&self.inner.owner_program, &self.inner.delegated.pubkey()).0;
        self.add_buffer_inner(buffer_key, data, wrong_owner)
    }

    fn add_buffer_inner(mut self, key: Pubkey, data: Vec<u8>, owner: Pubkey) -> Self {
        let lamports = 10_000_000;
        self.program_test.add_account(
            key,
            Account {
                lamports,
                data: data.clone(),
                owner,
                executable: false,
                rent_epoch: 0,
            },
        );
        self.buffer_account = Some((key, data, owner));
        self
    }

    async fn start(mut self) -> RunningScenario {
        // Ensure the payer has lamports for the delegation_record rent.
        self.program_test.add_account(
            self.inner.payer.pubkey(),
            Account {
                lamports: 1_000_000_000,
                data: vec![],
                owner: system_program::id(),
                executable: false,
                rent_epoch: 0,
            },
        );
        let context = self.program_test.start_with_context().await;
        RunningScenario {
            inner: self.inner,
            context,
            buffer_pubkey: self.buffer_account.map(|(k, _, _)| k),
        }
    }
}

struct RunningScenario {
    inner: DelegateScenario,
    context: ProgramTestContext,
    buffer_pubkey: Option<Pubkey>,
}

impl RunningScenario {
    async fn delegate(&mut self) -> Result<(), solana_program_test::BanksClientError> {
        let banks: &mut BanksClient = &mut self.context.banks_client;
        let blockhash = banks.get_latest_blockhash().await.unwrap();

        let (delegation_record, _) = find_delegation_record_pda(&self.inner.delegated.pubkey());

        let ix = build_delegate_ix(
            &self.inner.payer.pubkey(),
            &self.inner.delegated.pubkey(),
            &self.inner.owner_program,
            &delegation_record,
            self.inner.grid_id,
            self.buffer_pubkey.as_ref(),
        );

        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.inner.payer.pubkey()),
            &[&self.inner.payer, &self.inner.delegated],
            blockhash,
        );
        banks.process_transaction(tx).await
    }

    async fn delegate_without_delegated_signer(
        &mut self,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let banks: &mut BanksClient = &mut self.context.banks_client;
        let blockhash = banks.get_latest_blockhash().await.unwrap();
        let (delegation_record, _) = find_delegation_record_pda(&self.inner.delegated.pubkey());

        // Build the ix manually with delegated_account NOT marked as signer.
        let mut ix = build_delegate_ix(
            &self.inner.payer.pubkey(),
            &self.inner.delegated.pubkey(),
            &self.inner.owner_program,
            &delegation_record,
            self.inner.grid_id,
            self.buffer_pubkey.as_ref(),
        );
        ix.accounts[1].is_signer = false;

        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.inner.payer.pubkey()),
            &[&self.inner.payer],
            blockhash,
        );
        banks.process_transaction(tx).await
    }
}

// ===========================================================================================
// Flow 1: Keypair-wallet delegation (existing behavior, must remain unchanged)
// ===========================================================================================

#[tokio::test]
async fn delegate_keypair_wallet_succeeds() {
    // Pre-stage: delegated_account is empty (zero data) and Portal-owned, simulating
    // the post-system::Assign state for the keypair-wallet flow.
    let scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);

    let mut running = scenario.start().await;
    running
        .delegate()
        .await
        .expect("keypair-wallet delegate should succeed");

    let (delegation_record_pda, _) = find_delegation_record_pda(&running.inner.delegated.pubkey());
    let acct = running
        .context
        .banks_client
        .get_account(delegation_record_pda)
        .await
        .unwrap()
        .expect("delegation_record should exist");

    let record = DelegationRecord::try_from_slice(&acct.data).unwrap();
    assert_eq!(record.discriminator, DelegationRecord::DISCRIMINATOR);
    assert_eq!(record.owner_program, running.inner.owner_program.to_bytes());
    assert_eq!(record.grid_id, running.inner.grid_id);
}

#[tokio::test]
async fn delegate_keypair_wallet_requires_delegated_signer() {
    let scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);

    let mut running = scenario.start().await;
    let result = running.delegate_without_delegated_signer().await;
    assert!(
        result.is_err(),
        "delegate without delegated_account signer should fail"
    );
}

#[tokio::test]
async fn delegate_keypair_wallet_already_delegated_rejects() {
    let scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);

    let mut running = scenario.start().await;
    running
        .delegate()
        .await
        .expect("first delegate should succeed");

    let result = running.delegate().await;
    assert!(
        result.is_err(),
        "second delegate on the same account should fail"
    );
}

#[tokio::test]
async fn delegate_rejects_when_delegated_not_portal_owned() {
    // delegated_account is owned by system_program, not Portal. Should fail with
    // DelegatedAccountOwnerMismatch.
    let scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], system_program::id());

    let mut running = scenario.start().await;
    let result = running.delegate().await;
    assert!(
        result.is_err(),
        "delegate of system-owned account should fail (must be Portal-owned at CPI time)"
    );
}

// ===========================================================================================
// Flow 2: PDA-with-buffer delegation (new buffer-dance flow for stateful PDAs)
// ===========================================================================================

#[tokio::test]
async fn delegate_pda_with_buffer_restores_data() {
    // Pre-stage: delegated_account is Portal-owned but zeroed (post-dance), with 188 bytes
    // of capacity. Buffer at the expected derivation, owned by owner_program, contains 188
    // bytes of "real" data we expect to see in delegated_account after Delegate runs.
    let real_data: Vec<u8> = (0..188).map(|i| i as u8 ^ 0x42).collect();
    let scenario = StagedScenario::new(DelegateScenario::new())
        .with_delegated(vec![0u8; 188], PORTAL_PROGRAM_ID)
        .with_buffer(real_data.clone());

    let mut running = scenario.start().await;
    running
        .delegate()
        .await
        .expect("PDA-with-buffer delegate should succeed");

    // Verify delegation_record is created.
    let (delegation_record_pda, _) = find_delegation_record_pda(&running.inner.delegated.pubkey());
    let record_acct = running
        .context
        .banks_client
        .get_account(delegation_record_pda)
        .await
        .unwrap()
        .expect("delegation_record should exist");
    let record = DelegationRecord::try_from_slice(&record_acct.data).unwrap();
    assert_eq!(record.owner_program, running.inner.owner_program.to_bytes());

    // Verify delegated_account now holds the buffer's data.
    let delegated_acct = running
        .context
        .banks_client
        .get_account(running.inner.delegated.pubkey())
        .await
        .unwrap()
        .expect("delegated_account still exists");
    assert_eq!(
        delegated_acct.data, real_data,
        "buffer data should have been copied into delegated_account"
    );
    assert_eq!(delegated_acct.owner, PORTAL_PROGRAM_ID);
}

#[tokio::test]
async fn delegate_pda_buffer_at_wrong_pda_rejects() {
    let scenario = StagedScenario::new(DelegateScenario::new())
        .with_delegated(vec![0u8; 188], PORTAL_PROGRAM_ID)
        .with_buffer_at_wrong_pda(vec![0xAA; 188]);

    let mut running = scenario.start().await;
    let result = running.delegate().await;
    assert!(
        result.is_err(),
        "buffer at non-PDA address should be rejected"
    );
}

#[tokio::test]
async fn delegate_pda_buffer_wrong_owner_rejects() {
    // Buffer at correct derivation, but owned by system_program rather than owner_program.
    let scenario = StagedScenario::new(DelegateScenario::new())
        .with_delegated(vec![0u8; 188], PORTAL_PROGRAM_ID)
        .with_buffer_wrong_owner(vec![0xAA; 188], system_program::id());

    let mut running = scenario.start().await;
    let result = running.delegate().await;
    assert!(
        result.is_err(),
        "buffer not owned by owner_program should be rejected"
    );
}

#[tokio::test]
async fn delegate_pda_buffer_size_mismatch_rejects() {
    // delegated_account is 188 bytes; buffer is 100 bytes.
    let scenario = StagedScenario::new(DelegateScenario::new())
        .with_delegated(vec![0u8; 188], PORTAL_PROGRAM_ID)
        .with_buffer(vec![0xAA; 100]);

    let mut running = scenario.start().await;
    let result = running.delegate().await;
    assert!(
        result.is_err(),
        "buffer/delegated size mismatch should be rejected"
    );
}

#[tokio::test]
async fn delegate_pda_with_empty_buffer_behaves_like_keypair_flow() {
    // delegated_account is empty; buffer is empty too. The data-copy is a no-op but
    // the call should still succeed (degenerate case).
    let scenario = StagedScenario::new(DelegateScenario::new())
        .with_delegated(vec![], PORTAL_PROGRAM_ID)
        .with_buffer(vec![]);

    let mut running = scenario.start().await;
    running
        .delegate()
        .await
        .expect("delegate with degenerate empty buffer should succeed");
}

// ===========================================================================================
// Undelegate flows
// ===========================================================================================

impl RunningScenario {
    fn build_undelegate_ix(&self, owner_program: Pubkey) -> Instruction {
        let (delegation_record, _) = find_delegation_record_pda(&self.inner.delegated.pubkey());
        let ix = PortalInstruction::Undelegate;
        let data = borsh::to_vec(&ix).unwrap();
        Instruction {
            program_id: PORTAL_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(self.inner.payer.pubkey(), true),
                AccountMeta::new(self.inner.delegated.pubkey(), false),
                AccountMeta::new_readonly(owner_program, false),
                AccountMeta::new(delegation_record, false),
                AccountMeta::new_readonly(system_program::id(), false),
            ],
            data,
        }
    }

    async fn undelegate_with(
        &mut self,
        owner_program: Pubkey,
    ) -> Result<(), solana_program_test::BanksClientError> {
        let ix = self.build_undelegate_ix(owner_program);
        let banks: &mut BanksClient = &mut self.context.banks_client;
        let blockhash = banks.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.inner.payer.pubkey()),
            &[&self.inner.payer],
            blockhash,
        );
        banks.process_transaction(tx).await
    }

    async fn undelegate(&mut self) -> Result<(), solana_program_test::BanksClientError> {
        let owner = self.inner.owner_program;
        self.undelegate_with(owner).await
    }
}

#[tokio::test]
async fn undelegate_keypair_wallet_round_trip() {
    let scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);
    let mut running = scenario.start().await;
    running.delegate().await.expect("delegate should succeed");
    running
        .undelegate()
        .await
        .expect("undelegate should succeed");

    // delegated_account should now be owned by owner_program (no data, since keypair flow).
    let acct = running
        .context
        .banks_client
        .get_account(running.inner.delegated.pubkey())
        .await
        .unwrap()
        .expect("delegated_account still exists");
    assert_eq!(
        acct.owner, running.inner.owner_program,
        "ownership should revert to owner_program"
    );
    assert!(acct.data.is_empty() || acct.data.iter().all(|&b| b == 0));

    // delegation_record should be drained.
    let (delegation_record_pda, _) = find_delegation_record_pda(&running.inner.delegated.pubkey());
    let dr = running
        .context
        .banks_client
        .get_account(delegation_record_pda)
        .await
        .unwrap();
    if let Some(dr_acct) = dr {
        assert_eq!(dr_acct.lamports, 0, "delegation_record lamports drained");
        assert!(
            dr_acct.data.iter().all(|&b| b == 0),
            "delegation_record data zeroed"
        );
    }
}

#[tokio::test]
async fn undelegate_pda_with_data_round_trip() {
    // Delegate a PDA with 188 bytes of data, then undelegate. Verify ownership reverts
    // and data is zero-filled (caller's responsibility to re-install state).
    let real_data: Vec<u8> = (0..188).map(|i| i as u8 ^ 0x42).collect();
    let scenario = StagedScenario::new(DelegateScenario::new())
        .with_delegated(vec![0u8; 188], PORTAL_PROGRAM_ID)
        .with_buffer(real_data.clone());
    let mut running = scenario.start().await;
    running.delegate().await.expect("delegate should succeed");

    // After delegate, delegated_account holds the buffer's data. Verify.
    let acct_pre = running
        .context
        .banks_client
        .get_account(running.inner.delegated.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acct_pre.data, real_data);
    assert_eq!(acct_pre.owner, PORTAL_PROGRAM_ID);

    running
        .undelegate()
        .await
        .expect("undelegate should succeed");

    let acct_post = running
        .context
        .banks_client
        .get_account(running.inner.delegated.pubkey())
        .await
        .unwrap()
        .expect("account still exists");
    assert_eq!(
        acct_post.owner, running.inner.owner_program,
        "ownership reverts to owner_program"
    );
    assert!(
        acct_post.data.iter().all(|&b| b == 0),
        "delegated_account data zero-filled (owner program restores via follow-up ix)"
    );
    assert_eq!(
        acct_post.data.len(),
        188,
        "data length preserved across undelegate"
    );
}

#[tokio::test]
async fn undelegate_wrong_owner_program_rejects() {
    let scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);
    let mut running = scenario.start().await;
    running.delegate().await.expect("delegate should succeed");

    let bogus_owner = Pubkey::new_unique();
    let result = running.undelegate_with(bogus_owner).await;
    assert!(
        result.is_err(),
        "undelegate with wrong owner_program should fail"
    );
}

#[tokio::test]
async fn undelegate_non_delegated_account_rejects() {
    // Account exists, Portal-owned, but no DelegationRecord. Undelegate should fail.
    let scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);
    let mut running = scenario.start().await;
    let result = running.undelegate().await;
    assert!(
        result.is_err(),
        "undelegate without prior delegation should fail"
    );
}
