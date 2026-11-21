use {
    super::{CommittedAccountV2, Hash32, SettlementEffectV2, StateErrorV2},
    borsh::BorshDeserialize,
    northstar_token_bridge::{
        liability::{empty_flow, Entry, Flow},
        state::ErTokenAccount,
    },
    northstar_zk_types::continuity::IngressPayloadV2,
    solana_pubkey::Pubkey,
    std::collections::BTreeMap,
};

type Scope = (Hash32, Hash32, Hash32); // bridge program, session bridge, mint

fn holding(
    portal: &Pubkey,
    context: &Hash32,
    key: &Hash32,
    value: &CommittedAccountV2,
) -> Result<Option<(Scope, u64)>, StateErrorV2> {
    if value.lamports == 0
        || value.executable
        || value.data.len() != ErTokenAccount::SERIALIZED_LEN
        || value.data.first() != Some(&ErTokenAccount::DISCRIMINATOR)
    {
        return Ok(None);
    }
    let token =
        ErTokenAccount::try_from_slice(&value.data).map_err(|_| StateErrorV2::InvalidAccount)?;
    let registry = Pubkey::from(token.session_bridge);
    // Buffers contain byte-for-byte copies of holdings but are not claims. Other sessions
    // have their own ledger even when both sessions use the same bridge and mint.
    let (address, bump) = northstar_token_bridge::find_er_token_account_pda(
        &Pubkey::from(value.owner),
        &registry,
        &Pubkey::from(token.owner),
    );
    if address.to_bytes() != *key
        || northstar_portal::find_session_bridge_v2_pda(portal, context, &Pubkey::from(token.mint))
            .0
            != registry
    {
        return Ok(None);
    }
    if bump != token.bump {
        return Err(StateErrorV2::InvalidAccount);
    }
    Ok(Some((
        (value.owner, token.session_bridge, token.mint),
        token.amount,
    )))
}

fn record(
    flows: &mut BTreeMap<Scope, Flow>,
    scope: Scope,
    entry: Entry,
) -> Result<(), StateErrorV2> {
    let flow = flows.entry(scope).or_insert_with(empty_flow);
    *flow = flow.record(entry).ok_or(StateErrorV2::TokenLiability)?;
    Ok(())
}

// The caller first validates identical, sorted, unique pre/post keys against committed state.
// Untouched holdings cancel; only touched canonical accounts belong in this fold.
pub(super) fn check(
    portal: &Pubkey,
    context: &Hash32,
    pre: &[(Hash32, CommittedAccountV2)],
    post: &[(Hash32, CommittedAccountV2)],
    ingress: Option<&IngressPayloadV2>,
    effects: &[Vec<u8>],
) -> Result<(), StateErrorV2> {
    let mut flows = BTreeMap::new();
    let mut touched = BTreeMap::new();
    for ((key, before), (_, after)) in pre.iter().zip(post) {
        let old = holding(portal, context, key, before)?;
        let new = holding(portal, context, key, after)?;
        if let Some((scope, n)) = old {
            record(&mut flows, scope, Entry::Before(n))?;
        }
        if let Some((scope, n)) = new {
            if old.is_some_and(|(previous, _)| previous != scope) {
                return Err(StateErrorV2::InvalidAccount);
            }
            record(&mut flows, scope, Entry::After(n))?;
        }
        if let Some((scope, _)) = new.or(old) {
            touched.insert(*key, scope);
        }
    }
    if let Some(IngressPayloadV2::TokenDeposit {
        bridge,
        session_bridge,
        mint,
        account,
        amount,
        ..
    }) = ingress
    {
        let scope = (*bridge, *session_bridge, *mint);
        if touched.get(account) != Some(&scope) {
            return Err(StateErrorV2::TokenLiability);
        }
        record(&mut flows, scope, Entry::Ingress(*amount))?;
    }
    for effect in effects {
        if let SettlementEffectV2::TokenWithdrawal {
            bridge,
            session_bridge,
            account,
            amount,
            ..
        } =
            SettlementEffectV2::try_from_slice(effect).map_err(|_| StateErrorV2::InvalidAccounts)?
        {
            let scope = *touched.get(&account).ok_or(StateErrorV2::TokenLiability)?;
            if (scope.0, scope.1) != (bridge, session_bridge) {
                return Err(StateErrorV2::TokenLiability);
            }
            record(&mut flows, scope, Entry::Pending(amount))?;
        }
    }
    if flows.into_values().all(Flow::conserved) {
        Ok(())
    } else {
        Err(StateErrorV2::TokenLiability)
    }
}
