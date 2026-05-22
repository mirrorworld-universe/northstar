use {
    crate::ErStateDiff,
    northstar_portal::{
        BeginSettlement, FinishSettlement, MAX_SETTLEMENT_CHUNK, PortalInstruction,
        WriteSettlementChunk, find_delegation_record_pda,
    },
    solana_account::ReadableAccount,
    solana_clock::Slot,
    solana_instruction::{AccountMeta, Instruction},
    solana_pubkey::Pubkey,
    solana_sha256_hasher::Hasher,
    std::collections::HashSet,
};

const SETTLEMENT_CHECKSUM_DOMAIN: &[u8] = b"northstar-settlement-v0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementChunk {
    pub account: Pubkey,
    pub account_data_offset: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementPlan {
    pub er_slot: Slot,
    pub checksum: [u8; 32],
    pub chunks: Vec<SettlementChunk>,
}

impl SettlementPlan {
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub fn portal_instructions(
        &self,
        portal_program_id: Pubkey,
        session_pda: Pubkey,
        validator: Pubkey,
    ) -> Vec<Instruction> {
        if self.is_empty() {
            return vec![];
        }

        let mut instructions = Vec::with_capacity(self.chunks.len() + 2);
        instructions.push(Instruction {
            program_id: portal_program_id,
            accounts: vec![
                AccountMeta::new_readonly(validator, true),
                AccountMeta::new(session_pda, false),
            ],
            data: borsh::to_vec(&PortalInstruction::BeginSettlement(BeginSettlement {
                er_slot: self.er_slot,
                checksum: self.checksum,
            }))
            .unwrap(),
        });

        for chunk in &self.chunks {
            let mut chunk_data = [0; MAX_SETTLEMENT_CHUNK];
            chunk_data[..chunk.data.len()].copy_from_slice(&chunk.data);
            let (delegation_record, _) = find_delegation_record_pda(
                &portal_program_id.to_bytes(),
                &chunk.account.to_bytes(),
            );
            instructions.push(Instruction {
                program_id: portal_program_id,
                accounts: vec![
                    AccountMeta::new_readonly(validator, true),
                    AccountMeta::new_readonly(session_pda, false),
                    AccountMeta::new(chunk.account, false),
                    AccountMeta::new_readonly(Pubkey::new_from_array(delegation_record), false),
                ],
                data: borsh::to_vec(&PortalInstruction::WriteSettlementChunk(
                    WriteSettlementChunk {
                        er_slot: self.er_slot,
                        checksum: self.checksum,
                        account_data_offset: chunk.account_data_offset,
                        chunk_len: chunk.data.len() as u16,
                        chunk: chunk_data,
                    },
                ))
                .unwrap(),
            });
        }

        instructions.push(Instruction {
            program_id: portal_program_id,
            accounts: vec![
                AccountMeta::new_readonly(validator, true),
                AccountMeta::new(session_pda, false),
            ],
            data: borsh::to_vec(&PortalInstruction::FinishSettlement(FinishSettlement {
                er_slot: self.er_slot,
                checksum: self.checksum,
            }))
            .unwrap(),
        });
        instructions
    }
}

pub fn build_settlement_plan(
    diff: &ErStateDiff,
    delegated_accounts: &HashSet<Pubkey>,
    er_slot: Slot,
) -> Option<SettlementPlan> {
    let mut chunks = vec![];

    for account_diff in &diff.accounts {
        if !delegated_accounts.contains(&account_diff.pubkey) {
            continue;
        }
        let Some(l1_account) = account_diff.l1_account.as_ref() else {
            continue;
        };
        if l1_account.data().len() != account_diff.er_account.data().len() {
            continue;
        }

        chunks.extend(data_chunks_for_account(
            account_diff.pubkey,
            l1_account.data(),
            account_diff.er_account.data(),
        ));
    }

    if chunks.is_empty() {
        return None;
    }

    Some(SettlementPlan {
        er_slot,
        checksum: checksum_chunks(er_slot, &chunks),
        chunks,
    })
}

fn data_chunks_for_account(pubkey: Pubkey, l1_data: &[u8], er_data: &[u8]) -> Vec<SettlementChunk> {
    debug_assert_eq!(l1_data.len(), er_data.len());

    let mut chunks = vec![];
    let mut index = 0;
    while index < er_data.len() {
        if l1_data[index] == er_data[index] {
            index += 1;
            continue;
        }

        let range_start = index;
        index += 1;
        while index < er_data.len() && l1_data[index] != er_data[index] {
            index += 1;
        }

        for (chunk_index, data) in er_data[range_start..index]
            .chunks(MAX_SETTLEMENT_CHUNK)
            .enumerate()
        {
            chunks.push(SettlementChunk {
                account: pubkey,
                account_data_offset: (range_start + chunk_index * MAX_SETTLEMENT_CHUNK) as u32,
                data: data.to_vec(),
            });
        }
    }

    chunks
}

fn checksum_chunks(er_slot: Slot, chunks: &[SettlementChunk]) -> [u8; 32] {
    let mut hasher = Hasher::default();
    hasher.hash(SETTLEMENT_CHECKSUM_DOMAIN);
    hasher.hash(&er_slot.to_le_bytes());
    for chunk in chunks {
        hasher.hash(chunk.account.as_ref());
        hasher.hash(&chunk.account_data_offset.to_le_bytes());
        hasher.hash(&(chunk.data.len() as u32).to_le_bytes());
        hasher.hash(&chunk.data);
    }
    hasher.result().to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_data_produces_no_chunks() {
        let pubkey = Pubkey::new_unique();
        assert!(data_chunks_for_account(pubkey, &[1, 2, 3], &[1, 2, 3]).is_empty());
    }

    #[test]
    fn chunks_only_changed_ranges() {
        let pubkey = Pubkey::new_unique();
        let chunks = data_chunks_for_account(pubkey, &[0, 0, 0, 0, 0], &[0, 7, 8, 0, 9]);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].account, pubkey);
        assert_eq!(chunks[0].account_data_offset, 1);
        assert_eq!(chunks[0].data, vec![7, 8]);
        assert_eq!(chunks[1].account_data_offset, 4);
        assert_eq!(chunks[1].data, vec![9]);
    }

    #[test]
    fn large_changed_range_is_split() {
        let pubkey = Pubkey::new_unique();
        let l1_data = vec![0; MAX_SETTLEMENT_CHUNK + 1];
        let er_data = vec![1; MAX_SETTLEMENT_CHUNK + 1];
        let chunks = data_chunks_for_account(pubkey, &l1_data, &er_data);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].account_data_offset, 0);
        assert_eq!(chunks[0].data.len(), MAX_SETTLEMENT_CHUNK);
        assert_eq!(chunks[1].account_data_offset, MAX_SETTLEMENT_CHUNK as u32);
        assert_eq!(chunks[1].data.len(), 1);
    }

    #[test]
    fn empty_plan_emits_no_instructions() {
        let plan = SettlementPlan {
            er_slot: 1,
            checksum: [0; 32],
            chunks: vec![],
        };
        assert!(
            plan.portal_instructions(
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique()
            )
            .is_empty()
        );
    }
}
