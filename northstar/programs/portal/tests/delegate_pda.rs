//! Integration tests for `Portal::Delegate` (both flows) and `Portal::Undelegate`.
//!
//! `delegated_account` and `buffer` are pre-staged via `ProgramTest::add_account`
//! so the tests focus on Portal's validation + data-copy logic; the buffer dance
//! that the caller program performs is the caller's responsibility and not exercised
//! here.

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

fn build_delegate_ix(
    payer: &Pubkey,
    delegated_account: &Pubkey,
    owner_program: &Pubkey,
    delegation_record: &Pubkey,
    grid_id: u64,
    buffer: &Pubkey,
) -> Instruction {
    let ix = PortalInstruction::Delegate { grid_id };
    let data = borsh::to_vec(&ix).unwrap();

    Instruction {
        program_id: PORTAL_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(*delegated_account, true),
            AccountMeta::new_readonly(*owner_program, false),
            AccountMeta::new(*delegation_record, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new_readonly(*buffer, false),
        ],
        data,
    }
}

struct DelegateScenario {
    payer: Keypair,
    delegated: Keypair,
    buffer: Keypair,
    owner_program: Pubkey,
    grid_id: u64,
}

impl DelegateScenario {
    fn new() -> Self {
        Self {
            payer: Keypair::new(),
            delegated: Keypair::new(),
            buffer: Keypair::new(),
            owner_program: Pubkey::new_unique(),
            grid_id: 1,
        }
    }
}

struct StagedScenario {
    inner: DelegateScenario,
    program_test: ProgramTest,
}

impl StagedScenario {
    fn new(inner: DelegateScenario) -> Self {
        let mut program_test = ProgramTest::default();
        program_test.add_program("northstar_portal", PORTAL_PROGRAM_ID, None);
        Self {
            inner,
            program_test,
        }
    }

    fn with_delegated(mut self, data: Vec<u8>, owner: Pubkey) -> Self {
        self.program_test.add_account(
            self.inner.delegated.pubkey(),
            Account {
                lamports: 10_000_000,
                data,
                owner,
                executable: false,
                rent_epoch: 0,
            },
        );
        self
    }

    fn with_buffer(mut self, data: Vec<u8>, owner: Pubkey) -> Self {
        self.program_test.add_account(
            self.inner.buffer.pubkey(),
            Account {
                lamports: 10_000_000,
                data,
                owner,
                executable: false,
                rent_epoch: 0,
            },
        );
        self
    }

    async fn start(mut self) -> RunningScenario {
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
        }
    }
}

struct RunningScenario {
    inner: DelegateScenario,
    context: ProgramTestContext,
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
            &self.inner.buffer.pubkey(),
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

        let mut ix = build_delegate_ix(
            &self.inner.payer.pubkey(),
            &self.inner.delegated.pubkey(),
            &self.inner.owner_program,
            &delegation_record,
            self.inner.grid_id,
            &self.inner.buffer.pubkey(),
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

// ---- Delegate ----

#[tokio::test]
async fn delegate_keypair_wallet_succeeds() {
    let mut scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);
    let owner_program = scenario.inner.owner_program;
    scenario = scenario.with_buffer(vec![], owner_program);

    let mut running = scenario.start().await;
    running.delegate().await.expect("delegate should succeed");

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
async fn delegate_requires_delegated_signer() {
    let mut scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);
    let owner_program = scenario.inner.owner_program;
    scenario = scenario.with_buffer(vec![], owner_program);

    let mut running = scenario.start().await;
    let result = running.delegate_without_delegated_signer().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn delegate_already_delegated_rejects() {
    let mut scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);
    let owner_program = scenario.inner.owner_program;
    scenario = scenario.with_buffer(vec![], owner_program);

    let mut running = scenario.start().await;
    running
        .delegate()
        .await
        .expect("first delegate should succeed");

    let result = running.delegate().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn delegate_rejects_when_delegated_not_portal_owned() {
    let mut scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], system_program::id());
    let owner_program = scenario.inner.owner_program;
    scenario = scenario.with_buffer(vec![], owner_program);

    let mut running = scenario.start().await;
    let result = running.delegate().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn delegate_pda_with_buffer_restores_data() {
    let real_data: Vec<u8> = (0..188).map(|i| i as u8 ^ 0x42).collect();
    let mut scenario = StagedScenario::new(DelegateScenario::new())
        .with_delegated(vec![0u8; 188], PORTAL_PROGRAM_ID);
    let owner_program = scenario.inner.owner_program;
    scenario = scenario.with_buffer(real_data.clone(), owner_program);

    let mut running = scenario.start().await;
    running.delegate().await.expect("delegate should succeed");

    let delegated_acct = running
        .context
        .banks_client
        .get_account(running.inner.delegated.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delegated_acct.data, real_data);
    assert_eq!(delegated_acct.owner, PORTAL_PROGRAM_ID);
}

#[tokio::test]
async fn delegate_buffer_wrong_owner_rejects() {
    let mut scenario = StagedScenario::new(DelegateScenario::new())
        .with_delegated(vec![0u8; 188], PORTAL_PROGRAM_ID);
    // Buffer owned by system_program rather than owner_program.
    scenario = scenario.with_buffer(vec![0xAA; 188], system_program::id());

    let mut running = scenario.start().await;
    let result = running.delegate().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn delegate_buffer_size_mismatch_rejects() {
    let mut scenario = StagedScenario::new(DelegateScenario::new())
        .with_delegated(vec![0u8; 188], PORTAL_PROGRAM_ID);
    let owner_program = scenario.inner.owner_program;
    scenario = scenario.with_buffer(vec![0xAA; 100], owner_program);

    let mut running = scenario.start().await;
    let result = running.delegate().await;
    assert!(result.is_err());
}

// ---- Undelegate ----

#[tokio::test]
async fn undelegate_keypair_wallet_round_trip() {
    let mut scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);
    let owner_program = scenario.inner.owner_program;
    scenario = scenario.with_buffer(vec![], owner_program);

    let mut running = scenario.start().await;
    running.delegate().await.expect("delegate should succeed");
    running
        .undelegate()
        .await
        .expect("undelegate should succeed");

    let acct = running
        .context
        .banks_client
        .get_account(running.inner.delegated.pubkey())
        .await
        .unwrap()
        .expect("delegated_account still exists");
    assert_eq!(acct.owner, running.inner.owner_program);
}

#[tokio::test]
async fn undelegate_pda_with_data_round_trip() {
    let real_data: Vec<u8> = (0..188).map(|i| i as u8 ^ 0x42).collect();
    let mut scenario = StagedScenario::new(DelegateScenario::new())
        .with_delegated(vec![0u8; 188], PORTAL_PROGRAM_ID);
    let owner_program = scenario.inner.owner_program;
    scenario = scenario.with_buffer(real_data.clone(), owner_program);

    let mut running = scenario.start().await;
    running.delegate().await.expect("delegate should succeed");

    let pre = running
        .context
        .banks_client
        .get_account(running.inner.delegated.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pre.data, real_data);
    assert_eq!(pre.owner, PORTAL_PROGRAM_ID);

    running
        .undelegate()
        .await
        .expect("undelegate should succeed");

    let post = running
        .context
        .banks_client
        .get_account(running.inner.delegated.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(post.owner, running.inner.owner_program);
    assert!(post.data.iter().all(|&b| b == 0));
    assert_eq!(post.data.len(), 188);
}

#[tokio::test]
async fn undelegate_wrong_owner_program_rejects() {
    let mut scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);
    let owner_program = scenario.inner.owner_program;
    scenario = scenario.with_buffer(vec![], owner_program);

    let mut running = scenario.start().await;
    running.delegate().await.unwrap();

    let result = running.undelegate_with(Pubkey::new_unique()).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn undelegate_non_delegated_account_rejects() {
    let mut scenario =
        StagedScenario::new(DelegateScenario::new()).with_delegated(vec![], PORTAL_PROGRAM_ID);
    let owner_program = scenario.inner.owner_program;
    scenario = scenario.with_buffer(vec![], owner_program);

    let mut running = scenario.start().await;
    let result = running.undelegate().await;
    assert!(result.is_err());
}
