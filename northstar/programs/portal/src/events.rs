use {
    base64_no_std::{prelude::BASE64_STANDARD, Engine as _},
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::pubkey::Pubkey,
};

pub const TRANSFER_EVENT_LOG_PREFIX: &str = "Transfer Data: ";
pub const TRANSFER_EVENT_SERIALIZED_LEN: usize = 106;
pub const TRANSFER_EVENT_BASE64_LEN: usize = TRANSFER_EVENT_SERIALIZED_LEN.div_ceil(3) * 4;

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[borsh(use_discriminant = true)]
#[repr(u8)]
pub enum TransferEventKind {
    Deposit = 0,
    Withdrawal = 1,
}

impl TransferEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Withdrawal => "withdrawal",
        }
    }
}

#[cfg_attr(feature = "idl", derive(shank::ShankType))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct NorthstarTransferEvent {
    pub version: u8,
    pub kind: TransferEventKind,
    pub from: Pubkey,
    pub to: Pubkey,
    pub lamports: u64,
    pub pre_balance: u64,
    pub post_balance: u64,
    pub slot: u64,
    pub timestamp: i64,
}

impl NorthstarTransferEvent {
    pub const VERSION: u8 = 1;

    pub fn serialize_data(&self) -> [u8; TRANSFER_EVENT_SERIALIZED_LEN] {
        let mut data = [0u8; TRANSFER_EVENT_SERIALIZED_LEN];
        let mut cursor = &mut data[..];
        BorshSerialize::serialize(self, &mut cursor)
            .expect("transfer event serialization should not fail");
        debug_assert!(cursor.is_empty());
        data
    }
}

pub fn encode_transfer_event_data(
    event: &NorthstarTransferEvent,
    output: &mut [u8; TRANSFER_EVENT_BASE64_LEN],
) {
    let data = event.serialize_data();
    let encoded_len = BASE64_STANDARD
        .encode_slice(data, output)
        .expect("transfer event base64 buffer should be large enough");
    debug_assert_eq!(encoded_len, TRANSFER_EVENT_BASE64_LEN);
}

#[cfg(not(target_os = "solana"))]
pub fn transfer_event_data_log(event: &NorthstarTransferEvent) -> alloc::string::String {
    let mut encoded = [0u8; TRANSFER_EVENT_BASE64_LEN];
    encode_transfer_event_data(event, &mut encoded);
    let mut log = alloc::string::String::from(TRANSFER_EVENT_LOG_PREFIX);
    log.push_str(core::str::from_utf8(&encoded).unwrap());
    log
}

#[cfg(not(target_os = "solana"))]
pub fn emit_transfer_event(event: &NorthstarTransferEvent) {
    let _ = event;
}

#[cfg(target_os = "solana")]
pub fn emit_transfer_event(event: &NorthstarTransferEvent) {
    use pinocchio_log::logger::Logger;

    pinocchio_log::log!(
        "NorthstarTransferEvent kind={} lamports={} pre_balance={} post_balance={} slot={} \
         timestamp={}",
        event.kind.as_str(),
        event.lamports,
        event.pre_balance,
        event.post_balance,
        event.slot,
        event.timestamp
    );
    pinocchio_log::log!("NorthstarTransferEvent from");
    pinocchio::pubkey::log(&event.from);
    pinocchio_log::log!("NorthstarTransferEvent to");
    pinocchio::pubkey::log(&event.to);

    let mut encoded = [0u8; TRANSFER_EVENT_BASE64_LEN];
    encode_transfer_event_data(event, &mut encoded);
    let encoded = core::str::from_utf8(&encoded).unwrap();

    let mut logger = Logger::<{ 15 + TRANSFER_EVENT_BASE64_LEN }>::default();
    logger.append(TRANSFER_EVENT_LOG_PREFIX);
    logger.append(encoded);
    logger.log();
}
