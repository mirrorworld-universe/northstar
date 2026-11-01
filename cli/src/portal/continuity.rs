use {
    super::{find_fee_vault_pda, find_session_pda},
    crate::cli::CliError,
    clap::{App, Arg, ArgMatches},
    northstar_portal::{OpenSession, OpenSessionV2, PortalInstruction, SessionV2},
    solana_account::Account,
    solana_clap_utils::{input_parsers::value_of, input_validators::is_parsable},
    solana_hash::Hash,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
    solana_rpc_client::nonblocking::rpc_client::RpcClient,
    solana_sdk_ids::system_program,
    solana_sha256_hasher::hashv,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuitySessionArgs {
    pub l1_genesis: Hash,
    pub profile: Hash,
}

#[derive(Debug)]
pub(super) struct L1SessionV2 {
    portal: Pubkey,
    session: Pubkey,
    frame: SessionV2,
    context: [u8; 32],
    next_ingress: Pubkey,
}

impl L1SessionV2 {
    fn from_account(portal: Pubkey, account: &Account, genesis: Hash) -> Result<Self, CliError> {
        let frame: SessionV2 = borsh::from_slice(&account.data)
            .map_err(|_| CliError::BadParameter("Expected a continuity session account".into()))?;
        if account.owner != portal || account.executable || !frame.is_valid() {
            return Err(CliError::BadParameter(
                "Continuity session owner or layout differs".into(),
            ));
        }
        verify_genesis(Hash::new_from_array(frame.l1_genesis), genesis)?;
        let (session, bump) = northstar_portal::find_session_pda(&portal);
        if frame.base.bump != bump {
            return Err(CliError::BadParameter(
                "Continuity session bump differs".into(),
            ));
        }
        let context = frame
            .identity(&portal, &session)
            .commitment(|parts| hashv(parts).to_bytes())
            .map_err(|error| {
                CliError::BadParameter(format!("Invalid continuity identity: {error:?}"))
            })?;
        let sequence = frame
            .ingress_count
            .checked_add(1)
            .ok_or_else(|| CliError::BadParameter("Continuity ingress counter exhausted".into()))?;
        Ok(Self {
            portal,
            session,
            frame,
            context,
            next_ingress: northstar_portal::find_ingress_record_v2_pda(&portal, &context, sequence)
                .0,
        })
    }
}

pub(super) async fn load_session(
    rpc: &RpcClient,
    portal: Pubkey,
    sign_only: bool,
) -> Result<L1SessionV2, Box<dyn std::error::Error>> {
    if sign_only {
        return Err(CliError::BadParameter(
            "Offline continuity inputs require an anchored ingress context; use online mode".into(),
        )
        .into());
    }
    let account = rpc.get_account(&find_session_pda(&portal)).await?;
    Ok(L1SessionV2::from_account(
        portal,
        &account,
        rpc.get_genesis_hash().await?,
    )?)
}

pub(super) fn deposit_instruction(
    state: &L1SessionV2,
    depositor: Pubkey,
    recipient: Pubkey,
    lamports: u64,
) -> Result<Instruction, CliError> {
    if lamports == 0 || recipient == northstar_portal::WITHDRAWAL_SINK {
        return Err(CliError::BadParameter(
            "Invalid continuity deposit recipient or amount".into(),
        ));
    }
    Ok(Instruction {
        program_id: state.portal,
        accounts: vec![
            AccountMeta::new(depositor, true),
            AccountMeta::new(state.session, false),
            AccountMeta::new(
                northstar_portal::find_deposit_receipt_v2_pda(
                    &state.portal,
                    &state.context,
                    &recipient,
                )
                .0,
                false,
            ),
            AccountMeta::new_readonly(recipient, false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(state.next_ingress, false),
        ],
        data: borsh::to_vec(&PortalInstruction::DepositFeeV2 { lamports }).unwrap(),
    })
}

pub(super) fn delegate_instruction(
    state: &L1SessionV2,
    payer: Pubkey,
    account: Pubkey,
    owner: Pubkey,
    buffer: Pubkey,
    grid_id: u64,
) -> Result<Instruction, CliError> {
    if grid_id != state.frame.base.grid_id {
        return Err(CliError::BadParameter(
            "Delegation grid differs from continuity session".into(),
        ));
    }
    Ok(Instruction {
        program_id: state.portal,
        accounts: vec![
            AccountMeta::new(payer, true),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(state.session, false),
            AccountMeta::new(account, true),
            AccountMeta::new_readonly(owner, false),
            AccountMeta::new(
                northstar_portal::find_delegation_record_pda(&state.portal, &account).0,
                false,
            ),
            AccountMeta::new_readonly(buffer, false),
            AccountMeta::new(state.next_ingress, false),
        ],
        data: borsh::to_vec(&PortalInstruction::DelegateV2 { grid_id }).unwrap(),
    })
}

pub(super) fn open_args<'a, 'b>(app: App<'a, 'b>) -> App<'a, 'b> {
    app.arg(
        Arg::with_name("checkpoint_profile")
            .long("checkpoint-profile")
            .takes_value(true)
            .requires("l1_genesis")
            .validator(is_parsable::<Hash>)
            .value_name("HASH")
            .help("Pinned continuity execution profile (base58, 32 bytes)"),
    )
    .arg(
        Arg::with_name("l1_genesis")
            .long("l1-genesis")
            .takes_value(true)
            .requires("checkpoint_profile")
            .validator(is_parsable::<Hash>)
            .value_name("HASH")
            .help("Expected full L1 genesis hash; checked online before signing"),
    )
}

pub(super) fn parse_open_args(
    matches: &ArgMatches<'_>,
) -> Result<Option<ContinuitySessionArgs>, CliError> {
    if !matches.is_present("checkpoint_profile") && !matches.is_present("l1_genesis") {
        return Ok(None);
    }
    let l1_genesis = value_of(matches, "l1_genesis")
        .ok_or_else(|| CliError::BadParameter("Continuity requires --l1-genesis".into()))?;
    let profile = value_of(matches, "checkpoint_profile")
        .ok_or_else(|| CliError::BadParameter("Continuity requires --checkpoint-profile".into()))?;
    if l1_genesis == Hash::default() || profile == Hash::default() {
        return Err(CliError::BadParameter(
            "Continuity identities must be nonzero".into(),
        ));
    }
    Ok(Some(ContinuitySessionArgs {
        l1_genesis,
        profile,
    }))
}

pub(super) fn verify_genesis(expected: Hash, actual: Hash) -> Result<(), CliError> {
    if expected != actual {
        return Err(CliError::BadParameter(
            "L1 genesis differs from session identity".into(),
        ));
    }
    Ok(())
}

pub(super) fn open_instruction(
    portal: Pubkey,
    owner: Pubkey,
    session: OpenSession,
    identity: ContinuitySessionArgs,
) -> Instruction {
    Instruction {
        program_id: portal,
        accounts: vec![
            AccountMeta::new(owner, true),
            AccountMeta::new(find_session_pda(&portal), false),
            AccountMeta::new(find_fee_vault_pda(&portal), false),
            AccountMeta::new_readonly(system_program::id(), false),
            AccountMeta::new(
                northstar_portal::find_session_epoch_v2_pda(&portal).0,
                false,
            ),
        ],
        data: borsh::to_vec(&PortalInstruction::OpenSessionV2(OpenSessionV2 {
            session,
            l1_genesis: identity.l1_genesis.to_bytes(),
            profile: identity.profile.to_bytes(),
        }))
        .unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_account(portal: Pubkey) -> Account {
        let owner = Pubkey::new_unique();
        let frame = SessionV2 {
            discriminator: SessionV2::DISCRIMINATOR,
            base: northstar_portal::Session {
                discriminator: northstar_portal::Session::DISCRIMINATOR,
                grid_id: 7,
                ttl_slots: 1_000,
                fee_cap: 0,
                created_at: 10,
                nonce: 2,
                authority: owner,
                validator: owner,
                settlement_interval_slots: 10,
                last_settled_l1_slot: 0,
                last_settled_er_slot: 0,
                settlement_status: northstar_portal::SettlementStatus::Idle,
                settlement_er_slot: 0,
                settlement_checksum: [0; 32],
                settlement_accumulator: [0; 32],
                settlement_started_l1_slot: 0,
                bump: northstar_portal::find_session_pda(&portal).1,
            },
            l1_genesis: [1; 32],
            profile: [2; 32],
            ingress_count: 3,
            ingress_root: [3; 32],
            consumed_ingress_count: 0,
            consumed_ingress_root: [0; 32],
        };
        Account {
            lamports: 10_000_000,
            data: borsh::to_vec(&frame).unwrap(),
            owner: portal,
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn regression_v2_deposit_binds_receipt_and_next_ingress_to_the_nonce() {
        let portal = Pubkey::new_unique();
        let mut account = session_account(portal);
        let state =
            L1SessionV2::from_account(portal, &account, Hash::new_from_array([1; 32])).unwrap();
        let payer = Pubkey::new_unique();
        let recipient = Pubkey::new_unique();
        let instruction = deposit_instruction(&state, payer, recipient, 42).unwrap();
        assert!(matches!(
            borsh::from_slice::<PortalInstruction>(&instruction.data).unwrap(),
            PortalInstruction::DepositFeeV2 { lamports: 42 }
        ));
        assert_eq!(instruction.accounts.len(), 6);
        assert_eq!(
            instruction.accounts[1],
            AccountMeta::new(state.session, false)
        );
        assert_eq!(
            instruction.accounts[2],
            AccountMeta::new(
                northstar_portal::find_deposit_receipt_v2_pda(&portal, &state.context, &recipient)
                    .0,
                false
            )
        );
        assert_eq!(
            instruction.accounts[5],
            AccountMeta::new(
                northstar_portal::find_ingress_record_v2_pda(&portal, &state.context, 4).0,
                false
            )
        );
        let mut frame: SessionV2 = borsh::from_slice(&account.data).unwrap();
        frame.base.nonce += 1;
        account.data = borsh::to_vec(&frame).unwrap();
        let next =
            L1SessionV2::from_account(portal, &account, Hash::new_from_array([1; 32])).unwrap();
        let changed = deposit_instruction(&next, payer, recipient, 42).unwrap();
        assert_ne!(instruction.accounts[2].pubkey, changed.accounts[2].pubkey);
        assert_ne!(instruction.accounts[5].pubkey, changed.accounts[5].pubkey);
        assert!(deposit_instruction(&state, payer, recipient, 0).is_err());
        assert!(deposit_instruction(&state, payer, northstar_portal::WITHDRAWAL_SINK, 42).is_err());
    }

    #[test]
    fn regression_v2_delegation_binds_grid_and_writable_ingress_accounts() {
        let portal = Pubkey::new_unique();
        let state = L1SessionV2::from_account(
            portal,
            &session_account(portal),
            Hash::new_from_array([1; 32]),
        )
        .unwrap();
        let payer = Pubkey::new_unique();
        let delegated = Pubkey::new_unique();
        let buffer = Pubkey::new_unique();
        let instruction =
            delegate_instruction(&state, payer, delegated, system_program::id(), buffer, 7)
                .unwrap();
        assert!(matches!(
            borsh::from_slice::<PortalInstruction>(&instruction.data).unwrap(),
            PortalInstruction::DelegateV2 { grid_id: 7 }
        ));
        assert_eq!(instruction.accounts.len(), 8);
        assert_eq!(
            instruction.accounts[2],
            AccountMeta::new(state.session, false)
        );
        assert_eq!(instruction.accounts[3], AccountMeta::new(delegated, true));
        assert_eq!(
            instruction.accounts[7],
            AccountMeta::new(state.next_ingress, false)
        );
        assert!(
            delegate_instruction(&state, payer, delegated, system_program::id(), buffer, 8)
                .is_err()
        );
    }

    #[test]
    fn regression_v2_session_input_rejects_changed_bindings_and_exhausted_ingress() {
        let portal = Pubkey::new_unique();
        let account = session_account(portal);
        for mutation in 0..6 {
            let mut changed = account.clone();
            match mutation {
                0 => changed.owner = Pubkey::new_unique(),
                1 => changed.executable = true,
                2 => changed.data.push(0),
                _ => {
                    let mut frame: SessionV2 = borsh::from_slice(&changed.data).unwrap();
                    match mutation {
                        3 => frame.base.bump = frame.base.bump.wrapping_add(1),
                        4 => frame.base.nonce = 0,
                        _ => frame.ingress_count = u64::MAX,
                    }
                    changed.data = borsh::to_vec(&frame).unwrap();
                }
            }
            assert!(
                L1SessionV2::from_account(portal, &changed, Hash::new_from_array([1; 32])).is_err(),
                "mutation {mutation} admitted"
            );
        }
    }

    #[tokio::test]
    async fn regression_v2_offline_ingress_cannot_fall_back_to_legacy_encoding() {
        let rpc = RpcClient::new_mock("fails".into());
        let error = load_session(&rpc, Pubkey::new_unique(), true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Offline continuity"));
    }

    #[test]
    fn regression_v2_network_binding_compares_the_complete_hash() {
        let expected = Hash::new_from_array([1; 32]);
        let mut other = expected.to_bytes();
        other[31] = 2;
        assert!(verify_genesis(expected, expected).is_ok());
        assert!(verify_genesis(expected, Hash::new_from_array(other)).is_err());
    }

    #[test]
    fn regression_v2_identity_requires_a_nonzero_pair() {
        let genesis = Hash::new_from_array([1; 32]).to_string();
        let zero = Hash::default().to_string();
        let only_profile = open_args(App::new("open")).get_matches_from_safe(vec![
            "open",
            "--checkpoint-profile",
            &genesis,
        ]);
        assert!(only_profile.is_err());
        let matches = open_args(App::new("open"))
            .get_matches_from_safe(vec![
                "open",
                "--checkpoint-profile",
                &zero,
                "--l1-genesis",
                &genesis,
            ])
            .unwrap();
        assert!(parse_open_args(&matches).is_err());
    }

    #[test]
    fn regression_v2_open_binds_genesis_profile_and_nonce_account() {
        let portal = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let identity = ContinuitySessionArgs {
            l1_genesis: Hash::new_from_array([1; 32]),
            profile: Hash::new_from_array([2; 32]),
        };
        let instruction = open_instruction(
            portal,
            owner,
            OpenSession {
                grid_id: 7,
                ttl_slots: 100,
                fee_cap: 0,
                validator: owner,
                settlement_interval_slots: 10,
            },
            identity,
        );
        let decoded: PortalInstruction = borsh::from_slice(&instruction.data).unwrap();
        let PortalInstruction::OpenSessionV2(args) = decoded else {
            panic!("versioned open required")
        };
        assert_eq!(args.l1_genesis, identity.l1_genesis.to_bytes());
        assert_eq!(args.profile, identity.profile.to_bytes());
        assert_eq!(args.session.grid_id, 7);
        assert_eq!(instruction.accounts.len(), 5);
        assert_eq!(
            instruction.accounts[4],
            AccountMeta::new(
                northstar_portal::find_session_epoch_v2_pda(&portal).0,
                false
            )
        );
    }
}
