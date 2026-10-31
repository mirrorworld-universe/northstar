use {
    super::{find_fee_vault_pda, find_session_pda},
    crate::cli::CliError,
    clap::{App, Arg, ArgMatches},
    northstar_portal::{OpenSession, OpenSessionV2, PortalInstruction},
    solana_clap_utils::{input_parsers::value_of, input_validators::is_parsable},
    solana_hash::Hash,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
    solana_sdk_ids::system_program,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuitySessionArgs {
    pub l1_genesis: Hash,
    pub profile: Hash,
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
