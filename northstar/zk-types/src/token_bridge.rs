use borsh::{BorshDeserialize, BorshSerialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ErTokenAccount {
    pub discriminator: u8,
    pub session_bridge: [u8; 32],
    pub owner: [u8; 32],
    pub mint: [u8; 32],
    pub amount: u64,
    pub bump: u8,
}

impl ErTokenAccount {
    pub const DISCRIMINATOR: u8 = 2;
    pub const SEED_PREFIX: &'static [u8] = b"er_token";

    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TokenDepositReceipt {
    pub discriminator: u8,
    pub session_bridge: [u8; 32],
    pub er_token_account: [u8; 32],
    pub balance: u64,
    pub withdrawn: u64,
    pub bump: u8,
}

impl TokenDepositReceipt {
    pub const DISCRIMINATOR: u8 = 3;
    pub const SEED_PREFIX: &'static [u8] = b"token_deposit";

    pub fn is_valid(&self) -> bool {
        self.discriminator == Self::DISCRIMINATOR
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TokenDepositProgress {
    pub discriminator: u8,
    pub balance: u64,
    pub bump: u8,
}

impl TokenDepositProgress {
    pub const ORIGIN_DISCRIMINATOR: u8 = 4;
    pub const CURSOR_DISCRIMINATOR: u8 = 5;
    pub const ORIGIN_SEED: &'static [u8] = b"token_deposit_origin";
    pub const CURSOR_SEED: &'static [u8] = b"token_deposit_cursor";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn er_token_account_wire_layout_is_stable() {
        let state = ErTokenAccount {
            discriminator: 2,
            session_bridge: [1; 32],
            owner: [2; 32],
            mint: [3; 32],
            amount: 42,
            bump: 254,
        };
        let mut bytes = borsh::to_vec(&state).unwrap();
        assert_eq!(bytes.len(), 106);
        assert_eq!(&bytes[1..33], &state.session_bridge);
        assert_eq!(&bytes[33..65], &state.owner);
        assert_eq!(&bytes[65..97], &state.mint);
        assert_eq!(&bytes[97..105], &42u64.to_le_bytes());
        assert_eq!(borsh::from_slice::<ErTokenAccount>(&bytes).unwrap(), state);
        bytes.push(0);
        assert!(borsh::from_slice::<ErTokenAccount>(&bytes).is_err());
        assert!(!ErTokenAccount {
            discriminator: 0,
            ..state
        }
        .is_valid());
    }

    #[test]
    fn token_receipt_and_progress_wire_layouts_are_stable() {
        let state = TokenDepositReceipt {
            discriminator: 3,
            session_bridge: [1; 32],
            er_token_account: [2; 32],
            balance: 42,
            withdrawn: 7,
            bump: 253,
        };
        let mut bytes = borsh::to_vec(&state).unwrap();
        assert_eq!(bytes.len(), 82);
        assert_eq!(&bytes[65..73], &42u64.to_le_bytes());
        assert_eq!(&bytes[73..81], &7u64.to_le_bytes());
        assert_eq!(
            borsh::from_slice::<TokenDepositReceipt>(&bytes).unwrap(),
            state
        );
        bytes.push(0);
        assert!(borsh::from_slice::<TokenDepositReceipt>(&bytes).is_err());
        assert!(!TokenDepositReceipt {
            discriminator: 0,
            ..state
        }
        .is_valid());
        let progress = TokenDepositProgress {
            discriminator: 4,
            balance: 42,
            bump: 252,
        };
        let mut encoded = borsh::to_vec(&progress).unwrap();
        assert_eq!(encoded.len(), 10);
        assert_eq!(&encoded[1..9], &42u64.to_le_bytes());
        assert_eq!(
            borsh::from_slice::<TokenDepositProgress>(&encoded).unwrap(),
            progress
        );
        encoded.push(0);
        assert!(borsh::from_slice::<TokenDepositProgress>(&encoded).is_err());
    }
}
