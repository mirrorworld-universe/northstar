//! Reconstruct token withdrawal occurrences without reading a live bank. The signature
//! authenticates the outer message, not CPI data or the bank's execution status.
use {
    super::{token_liability::Scope, *},
    bincode::Options,
    northstar_portal::{SessionBridge, SessionBridgeV2},
    northstar_token_bridge::{
        entitlement::{match_occurrences, Intent, Source},
        state::ErTokenAccount,
        trace_origin::{request, retain_parent, visible, withdrawal, Addresses, Origin},
    },
    solana_transaction::{versioned::VersionedTransaction, TransactionError},
};
mod lookups;
#[cfg(feature = "checkpoint-continuity-v2")]
pub(crate) use lookups::capture;

const INVALID: StateErrorV2 = StateErrorV2::TokenProvenance;
fn has_call(tx: &VersionedTransaction, inner: &[RecordedInnerInstructionV2]) -> bool {
    tx.message
        .instructions()
        .iter()
        .any(|i| withdrawal(&i.data).is_some())
        || inner.iter().any(|i| withdrawal(&i.data).is_some())
}

struct Evidence<'a> {
    record: &'a RecordedTransitionV2,
    before: &'a CommittedStateV2,
    after: &'a CommittedStateV2,
    touched: &'a BTreeMap<Hash32, Scope>,
    keys: Vec<Pubkey>,
    message: &'a solana_message::VersionedMessage,
}

/// A referenced account must open to the committed ER state or a single retained readonly L1 observation.
fn opens(
    state: &CommittedStateV2,
    record: &RecordedTransitionV2,
    key: &Pubkey,
    owner: &Pubkey,
    data: &[u8],
) -> bool {
    if let Some(v) = state.accounts().get(key.as_array()) {
        return v.lamports != 0 && !v.executable && v.owner == owner.to_bytes() && v.data == data;
    }
    let mut matches = record
        .readonly_l1_values
        .iter()
        .filter(|r| r.account == *key);
    matches
        .next()
        .is_some_and(|v| v.owner == *owner && v.lamports != 0 && v.data_hash == hash_parts(&[data]))
        && matches.next().is_none()
}
impl Evidence<'_> {
    fn call(
        &self,
        origin: Origin,
        program_index: u8,
        accounts: &[u8],
        data: &[u8],
    ) -> Result<Option<Intent>, StateErrorV2> {
        if withdrawal(data).is_none() {
            return Ok(None);
        }
        let [who, source, registry, portal, destination, token, mint, ..] = accounts else {
            return Ok(None);
        };
        let key = |i: u8| self.keys.get(usize::from(i)).copied().ok_or(INVALID);
        let bridge = key(program_index)?;
        let source_index = usize::from(*source);
        let source = key(*source)?;
        let (actor, registry, portal, destination, token, mint) = (
            key(*who)?,
            key(*registry)?,
            key(*portal)?,
            key(*destination)?,
            key(*token)?,
            key(*mint)?,
        );
        let scope = self
            .touched
            .get(source.as_array())
            .filter(|scope| scope.0 == bridge.to_bytes());
        let context = self.record.before.session_context;
        let session_portal = Pubkey::from(self.before.identity().portal);
        let (expected, bump) =
            northstar_portal::find_session_bridge_v2_pda(&session_portal, &context, &mint);
        let frame = SessionBridgeV2 {
            discriminator: SessionBridgeV2::DISCRIMINATOR,
            session_context: context,
            base: SessionBridge {
                discriminator: SessionBridge::DISCRIMINATOR,
                session: self.before.identity().session.into(),
                mint,
                bridge_program: bridge,
                vault: northstar_token_bridge::find_token_vault_pda(&bridge, &registry).0,
                token_program: token,
                bump,
            },
        };
        let registered = portal == session_portal
            && expected == registry
            && opens(
                self.before,
                self.record,
                &registry,
                &session_portal,
                &borsh::to_vec(&frame).map_err(|_| INVALID)?,
            );
        let Some(scope) = scope else {
            // Discover calls independently of the claimed writes and effects. An existing
            // canonical holding cannot disappear from both the write set and event list.
            let existing = self
                .before
                .accounts()
                .get(source.as_array())
                .map(|v| {
                    super::token_liability::holding(&session_portal, &context, source.as_array(), v)
                })
                .transpose()?
                .flatten();
            return if registered || existing.is_some_and(|(s, _)| s.0 == bridge.to_bytes()) {
                Err(INVALID)
            } else {
                Ok(None)
            };
        };
        if !registered
            || registry.to_bytes() != scope.1
            || mint.to_bytes() != scope.2
            || !self.message.is_maybe_writable_with_reserved_addresses(
                source_index,
                None::<&std::collections::BTreeSet<Pubkey>>,
            )
        {
            return Err(INVALID);
        }
        let holding: ErTokenAccount = borsh::from_slice(
            &self
                .after
                .accounts()
                .get(source.as_array())
                .ok_or(INVALID)?
                .data,
        )
        .map_err(|_| INVALID)?;
        let a = Addresses {
            actor: actor.as_array(),
            program: bridge.as_array(),
            registry: registry.as_array(),
            source: source.as_array(),
            destination: destination.as_array(),
            mint: mint.as_array(),
        };
        let h = Source {
            owner: &holding.owner,
            registry: &holding.session_bridge,
            mint: &holding.mint,
        };
        request(
            data,
            origin,
            self.message.header().num_required_signatures,
            *who,
            a,
            h,
        )
        .map(Some)
        .ok_or(INVALID)
    }
}

pub(super) fn verify(
    record: &RecordedTransitionV2,
    before: &CommittedStateV2,
    after: &CommittedStateV2,
    touched: &BTreeMap<Hash32, Scope>,
) -> Result<(), StateErrorV2> {
    let (transaction, execution_result, replay, pre, post) = match &record.payload {
        TransitionPayloadV2::Execution {
            transaction,
            execution_result,
            replay,
            pre,
            post,
        }
        | TransitionPayloadV2::TokenIngress {
            transaction,
            execution_result,
            replay,
            pre,
            post,
            ..
        } => (transaction, execution_result, replay, pre, post),
        TransitionPayloadV2::Ingress { .. } => return Err(INVALID),
    };
    let mut effects = Vec::new();
    for encoded in &record.settlement_effects {
        if let SettlementEffectV2::TokenWithdrawal {
            bridge,
            session_bridge,
            account,
            destination,
            amount,
            decimals,
            ..
        } = borsh::from_slice(encoded).map_err(|_| INVALID)?
        {
            effects.push(Intent {
                bridge,
                registry: session_bridge,
                source: account,
                mint: touched.get(&account).ok_or(INVALID)?.2,
                destination,
                amount,
                decimals,
            });
        }
    }
    let tx: VersionedTransaction = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(1232)
        .reject_trailing_bytes()
        .deserialize(transaction)
        .map_err(|_| INVALID)?;
    if execution_result.len() > crate::checkpoint::MAX_TRANSACTION_EFFECT_BYTES_V1 {
        return Err(INVALID);
    }
    let result: RecordedExecutionResultV2 =
        borsh::from_slice(execution_result).map_err(|_| INVALID)?;
    let status: Result<(), TransactionError> = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(1232)
        .reject_trailing_bytes()
        .deserialize(&result.status)
        .map_err(|_| INVALID)?;
    if status.is_err() {
        return if !matches!(record.payload, TransitionPayloadV2::TokenIngress { .. })
            && effects.is_empty()
            && pre
                .iter()
                .zip(post)
                .all(|((key, old), (_, new))| !touched.contains_key(key) || old == new)
        {
            Ok(())
        } else {
            Err(INVALID)
        };
    }
    if !has_call(&tx, &result.inner) {
        return if effects.is_empty() {
            Ok(())
        } else {
            Err(INVALID)
        };
    }
    if tx
        .message
        .instructions()
        .len()
        .saturating_add(result.inner.len())
        > solana_transaction_context::MAX_INSTRUCTION_TRACE_LENGTH
    {
        return Err(INVALID);
    }
    tx.verify_and_hash_message().map_err(|_| INVALID)?;
    // Retention de-duplicates transaction *bytes*. Only the canonical signed encoding qualifies.
    if bincode::serialize(&tx).map_err(|_| INVALID)? != *transaction {
        return Err(INVALID);
    }
    let keys = lookups::resolve(&tx, replay, |k, o, b| opens(before, record, k, o, b))?;
    let evidence = Evidence {
        record,
        before,
        after,
        touched,
        keys,
        message: &tx.message,
    };
    let mut calls = Vec::new();
    let mut trace = result.inner.iter().peekable();
    for (index, outer) in tx.message.instructions().iter().enumerate() {
        if outer.accounts.len() > solana_transaction_context::MAX_ACCOUNTS_PER_INSTRUCTION {
            return Err(INVALID);
        }
        calls.extend(evidence.call(
            Origin::Message,
            outer.program_id_index,
            &outer.accounts,
            &outer.data,
        )?);
        let mut stack = vec![outer.accounts.as_slice()];
        while let Some(inner) = trace.next_if(|i| i.outer_index as usize == index) {
            let keep = retain_parent(stack.len(), inner.stack_height).ok_or(INVALID)?;
            stack.truncate(keep);
            if inner.accounts.len() > solana_transaction_context::MAX_ACCOUNTS_PER_INSTRUCTION
                || usize::from(inner.program_id_index) >= evidence.keys.len()
                || !visible(&inner.accounts, stack.last().ok_or(INVALID)?)
            {
                return Err(INVALID);
            }
            calls.extend(evidence.call(
                Origin::Invocation,
                inner.program_id_index,
                &inner.accounts,
                &inner.data,
            )?);
            stack.push(&inner.accounts);
        }
    }
    if trace.next().is_some() || !match_occurrences(&calls, &effects) {
        return Err(INVALID);
    }
    Ok(())
}
