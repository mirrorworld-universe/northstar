use {
    super::invalid,
    crate::{
        continuity_v2::{RecordedTransitionV2, SettlementEffectV2, TransitionPayloadV2},
        settlement::TokenWithdrawalSettlement,
    },
    northstar_token_bridge::{
        entitlement::{from_payout, match_occurrences, next_ticket, Intent},
        state::ErTokenAccount,
    },
    std::{collections::BTreeMap, io},
};

/// Validate only the sealed prefix: the active ER can have unrelated successor withdrawals.
/// Mint is not duplicated in the wire effect. Recover it from the same committed source
/// holding used by the recorder, never from the proposed payout or the current L1 bank.
pub(super) fn validate(
    transitions: &[RecordedTransitionV2],
    payments: &[TokenWithdrawalSettlement],
) -> io::Result<()> {
    let mut requests = Vec::new();
    for transition in transitions {
        for bytes in &transition.settlement_effects {
            let SettlementEffectV2::TokenWithdrawal {
                bridge,
                session_bridge,
                account,
                destination,
                amount,
                decimals,
                ..
            } = borsh::from_slice(bytes)?
            else {
                continue;
            };
            let TransitionPayloadV2::Execution { post, .. } = &transition.payload else {
                return Err(invalid("withdrawal has no ER execution"));
            };
            let index = post
                .binary_search_by_key(&account, |(key, _)| *key)
                .map_err(|_| invalid("withdrawal has no touched holding"))?;
            let value = &post[index].1;
            let holding: ErTokenAccount = borsh::from_slice(&value.data)?;
            if value.owner != bridge
                || !holding.is_valid()
                || holding.session_bridge != session_bridge
            {
                return Err(invalid("withdrawal holding differs from sealed effect"));
            }
            requests.push(Intent {
                bridge,
                registry: session_bridge,
                source: account,
                mint: holding.mint,
                destination,
                amount,
                decimals,
            });
        }
    }
    let mut rows = Vec::with_capacity(payments.len());
    let mut frontiers = BTreeMap::new();
    for p in payments {
        let (vault, previous) = frontiers
            .entry((p.bridge_program, p.session_bridge))
            .or_insert_with(|| {
                (
                    northstar_token_bridge::find_token_vault_pda(
                        &p.bridge_program,
                        &p.session_bridge,
                    )
                    .0,
                    None,
                )
            });
        if *vault != p.vault || !next_ticket(*previous, p.amount, p.withdrawn) {
            return Err(invalid("withdrawal ticket differs from per-vault schedule"));
        }
        *previous = Some(p.withdrawn);
        rows.push(from_payout(&p.payout()));
    }
    requests.sort_unstable();
    rows.sort_unstable();
    if !match_occurrences(&requests, &rows) {
        return Err(invalid(
            "withdrawal plan differs from authorized occurrences",
        ));
    }
    Ok(())
}
