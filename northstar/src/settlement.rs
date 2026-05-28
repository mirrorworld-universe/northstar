use {
    crate::ErStateDiff,
    northstar_portal::{
        BeginSettlement, FinishSettlement, MAX_SETTLEMENT_CHUNK, PortalInstruction,
        SettleDepositReceipt, WriteSettlementChunk, find_delegation_record_pda,
    },
    solana_account::ReadableAccount,
    solana_clock::Slot,
    solana_hash::Hash,
    solana_instruction::{AccountMeta, Instruction},
    solana_keypair::Keypair,
    solana_packet::PACKET_DATA_SIZE,
    solana_pubkey::Pubkey,
    solana_sha256_hasher::Hasher,
    solana_signer::Signer,
    solana_transaction::Transaction,
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
pub struct ReceiptBalanceSettlement {
    pub recipient: Pubkey,
    pub balance: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementPlan {
    pub er_slot: Slot,
    pub checksum: [u8; 32],
    pub chunks: Vec<SettlementChunk>,
    pub receipt_balances: Vec<ReceiptBalanceSettlement>,
}

impl SettlementPlan {
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty() && self.receipt_balances.is_empty()
    }

    pub fn portal_transactions(
        &self,
        portal_program_id: Pubkey,
        session_pda: Pubkey,
        validator: &Keypair,
        recent_blockhash: Hash,
    ) -> Vec<Transaction> {
        self.portal_transactions_inner(
            portal_program_id,
            session_pda,
            validator,
            recent_blockhash,
            true,
        )
    }

    pub fn portal_retry_transactions_after_begin(
        &self,
        portal_program_id: Pubkey,
        session_pda: Pubkey,
        validator: &Keypair,
        recent_blockhash: Hash,
    ) -> Vec<Transaction> {
        self.portal_transactions_inner(
            portal_program_id,
            session_pda,
            validator,
            recent_blockhash,
            false,
        )
    }

    fn portal_transactions_inner(
        &self,
        portal_program_id: Pubkey,
        session_pda: Pubkey,
        validator: &Keypair,
        recent_blockhash: Hash,
        include_begin: bool,
    ) -> Vec<Transaction> {
        self.portal_instruction_batches(portal_program_id, session_pda, validator, include_begin)
            .into_iter()
            .map(|instructions| {
                sign_settlement_transaction(&instructions, validator, recent_blockhash)
            })
            .collect()
    }

    pub fn portal_instruction_batches(
        &self,
        portal_program_id: Pubkey,
        session_pda: Pubkey,
        validator: &Keypair,
        include_begin: bool,
    ) -> Vec<Vec<Instruction>> {
        split_settlement_instruction_batches(
            self.portal_instructions_inner(
                portal_program_id,
                session_pda,
                validator.pubkey(),
                include_begin,
            ),
            validator,
        )
    }

    pub fn portal_instructions(
        &self,
        portal_program_id: Pubkey,
        session_pda: Pubkey,
        validator: Pubkey,
    ) -> Vec<Instruction> {
        self.portal_instructions_inner(portal_program_id, session_pda, validator, true)
    }

    fn portal_instructions_inner(
        &self,
        portal_program_id: Pubkey,
        session_pda: Pubkey,
        validator: Pubkey,
        include_begin: bool,
    ) -> Vec<Instruction> {
        if self.is_empty() {
            return vec![];
        }

        let mut instructions = Vec::with_capacity(self.chunks.len() + 2);
        if include_begin {
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
        }

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
                    AccountMeta::new(session_pda, false),
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

        for receipt in &self.receipt_balances {
            let deposit_receipt = Pubkey::find_program_address(
                &[
                    b"deposit_receipt",
                    session_pda.as_ref(),
                    receipt.recipient.as_ref(),
                ],
                &portal_program_id,
            )
            .0;
            instructions.push(Instruction {
                program_id: portal_program_id,
                accounts: vec![
                    AccountMeta::new_readonly(validator, true),
                    AccountMeta::new(session_pda, false),
                    AccountMeta::new(deposit_receipt, false),
                    AccountMeta::new_readonly(receipt.recipient, false),
                ],
                data: borsh::to_vec(&PortalInstruction::SettleDepositReceipt(
                    SettleDepositReceipt {
                        er_slot: self.er_slot,
                        checksum: self.checksum,
                        balance: receipt.balance,
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

fn sign_settlement_transaction(
    instructions: &[Instruction],
    validator: &Keypair,
    recent_blockhash: Hash,
) -> Transaction {
    Transaction::new_signed_with_payer(
        instructions,
        Some(&validator.pubkey()),
        &[validator],
        recent_blockhash,
    )
}

fn settlement_transaction_size(instructions: &[Instruction], validator: &Keypair) -> usize {
    let transaction = sign_settlement_transaction(instructions, validator, Hash::default());
    bincode::serialized_size(&transaction).unwrap_or(u64::MAX) as usize
}

fn split_settlement_instruction_batches(
    instructions: Vec<Instruction>,
    validator: &Keypair,
) -> Vec<Vec<Instruction>> {
    let mut batches = vec![];
    let mut current = vec![];

    for instruction in instructions {
        let mut candidate = current.clone();
        candidate.push(instruction.clone());
        if settlement_transaction_size(&candidate, validator) <= PACKET_DATA_SIZE {
            current = candidate;
            continue;
        }

        if !current.is_empty() {
            batches.push(current);
        }
        current = vec![instruction];
    }

    if !current.is_empty() {
        batches.push(current);
    }

    batches
}

pub fn build_settlement_plan(
    diff: &ErStateDiff,
    delegated_accounts: &HashSet<Pubkey>,
    er_slot: Slot,
    receipt_balances: Vec<ReceiptBalanceSettlement>,
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

    if chunks.is_empty() && receipt_balances.is_empty() {
        return None;
    }

    Some(SettlementPlan {
        er_slot,
        checksum: checksum_settlement(er_slot, &chunks, &receipt_balances),
        chunks,
        receipt_balances,
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

fn checksum_settlement(
    er_slot: Slot,
    chunks: &[SettlementChunk],
    receipt_balances: &[ReceiptBalanceSettlement],
) -> [u8; 32] {
    let mut hasher = Hasher::default();
    hasher.hash(SETTLEMENT_CHECKSUM_DOMAIN);
    hasher.hash(&er_slot.to_le_bytes());
    for chunk in chunks {
        hasher.hash(b"data");
        hasher.hash(chunk.account.as_ref());
        hasher.hash(&chunk.account_data_offset.to_le_bytes());
        hasher.hash(&(chunk.data.len() as u32).to_le_bytes());
        hasher.hash(&chunk.data);
    }
    for receipt in receipt_balances {
        hasher.hash(b"receipt");
        hasher.hash(receipt.recipient.as_ref());
        hasher.hash(&receipt.balance.to_le_bytes());
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
            receipt_balances: vec![],
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

    #[test]
    fn large_settlement_plan_must_not_build_oversized_transaction() {
        let portal_program_id = Pubkey::new_unique();
        let session_pda = Pubkey::new_unique();
        let validator = Keypair::new();
        let plan = SettlementPlan {
            er_slot: 42,
            checksum: [7; 32],
            chunks: (0..3)
                .map(|index| SettlementChunk {
                    account: Pubkey::new_unique(),
                    account_data_offset: (index * MAX_SETTLEMENT_CHUNK) as u32,
                    data: vec![index as u8; MAX_SETTLEMENT_CHUNK],
                })
                .collect(),
            receipt_balances: vec![],
        };

        let transactions = plan.portal_transactions(
            portal_program_id,
            session_pda,
            &validator,
            Hash::new_unique(),
        );

        assert!(transactions.len() > 1, "large settlement should be split");
        for transaction in transactions {
            let transaction_size = bincode::serialized_size(&transaction).unwrap() as usize;
            assert!(
                transaction_size <= PACKET_DATA_SIZE,
                "settlement transaction size {transaction_size} exceeds packet limit \
                 {PACKET_DATA_SIZE}"
            );
        }
    }

    #[test]
    fn portal_transaction_keeps_settlement_instructions_atomic_and_ordered() {
        use borsh::BorshDeserialize;

        let portal_program_id = Pubkey::new_unique();
        let session_pda = Pubkey::new_unique();
        let validator = Keypair::new();
        let plan = SettlementPlan {
            er_slot: 42,
            checksum: [7; 32],
            chunks: vec![
                SettlementChunk {
                    account: Pubkey::new_unique(),
                    account_data_offset: 0,
                    data: vec![1, 2, 3],
                },
                SettlementChunk {
                    account: Pubkey::new_unique(),
                    account_data_offset: MAX_SETTLEMENT_CHUNK as u32,
                    data: vec![4],
                },
            ],
            receipt_balances: vec![ReceiptBalanceSettlement {
                recipient: Pubkey::new_unique(),
                balance: 9,
            }],
        };

        let transactions = plan.portal_transactions(
            portal_program_id,
            session_pda,
            &validator,
            Hash::new_unique(),
        );
        assert!(
            transactions
                .iter()
                .all(|transaction| transaction.signatures.len() == 1)
        );
        let instructions = transactions
            .iter()
            .flat_map(|transaction| transaction.message.instructions.iter())
            .collect::<Vec<_>>();

        assert_eq!(instructions.len(), 5);
        assert!(matches!(
            PortalInstruction::try_from_slice(&instructions[0].data).unwrap(),
            PortalInstruction::BeginSettlement(_)
        ));
        assert!(matches!(
            PortalInstruction::try_from_slice(&instructions[1].data).unwrap(),
            PortalInstruction::WriteSettlementChunk(_)
        ));
        assert!(matches!(
            PortalInstruction::try_from_slice(&instructions[2].data).unwrap(),
            PortalInstruction::WriteSettlementChunk(_)
        ));
        assert!(matches!(
            PortalInstruction::try_from_slice(&instructions[3].data).unwrap(),
            PortalInstruction::SettleDepositReceipt(_)
        ));
        assert!(matches!(
            PortalInstruction::try_from_slice(&instructions[4].data).unwrap(),
            PortalInstruction::FinishSettlement(_)
        ));
    }
}
