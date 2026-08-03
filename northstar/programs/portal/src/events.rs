use {
    base64_no_std::{prelude::BASE64_STANDARD, Engine as _},
    borsh::{BorshDeserialize, BorshSerialize},
    pinocchio::pubkey::Pubkey,
};

pub const TRANSFER_EVENT_LOG_PREFIX: &str = "Transfer Data: ";
pub const TRANSFER_EVENT_SERIALIZED_LEN: usize = 106;
pub const TRANSFER_EVENT_BASE64_LEN: usize = TRANSFER_EVENT_SERIALIZED_LEN.div_ceil(3) * 4;

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
fn log_transfer_event_data(event: &NorthstarTransferEvent) {
    const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut message = [0u8; TRANSFER_EVENT_LOG_PREFIX.len() + TRANSFER_EVENT_BASE64_LEN];
    message[..TRANSFER_EVENT_LOG_PREFIX.len()]
        .copy_from_slice(TRANSFER_EVENT_LOG_PREFIX.as_bytes());
    let output = &mut message[TRANSFER_EVENT_LOG_PREFIX.len()..];
    for group in 0..35 {
        let input = group * 3;
        let output_offset = group * 4;
        let a = transfer_event_byte(event, input);
        let b = transfer_event_byte(event, input + 1);
        let c = transfer_event_byte(event, input + 2);
        output_triplet(output, output_offset, a, b, c, BASE64);
    }
    let last = transfer_event_byte(event, TRANSFER_EVENT_SERIALIZED_LEN - 1);
    output[140] = BASE64[(last >> 2) as usize];
    output[141] = BASE64[((last & 0x03) << 4) as usize];
    output[142] = b'=';
    output[143] = b'=';
    pinocchio::log::sol_log(core::str::from_utf8(&message).unwrap());
}

#[cfg(target_os = "solana")]
fn output_triplet(output: &mut [u8], offset: usize, a: u8, b: u8, c: u8, base64: &[u8; 64]) {
    output[offset] = base64[(a >> 2) as usize];
    output[offset + 1] = base64[(((a & 0x03) << 4) | (b >> 4)) as usize];
    output[offset + 2] = base64[(((b & 0x0f) << 2) | (c >> 6)) as usize];
    output[offset + 3] = base64[(c & 0x3f) as usize];
}

#[cfg(target_os = "solana")]
fn transfer_event_byte(event: &NorthstarTransferEvent, index: usize) -> u8 {
    match index {
        0 => event.version,
        1 => event.kind as u8,
        2..=33 => event.from[index - 2],
        34..=65 => event.to[index - 34],
        66..=73 => event.lamports.to_le_bytes()[index - 66],
        74..=81 => event.pre_balance.to_le_bytes()[index - 74],
        82..=89 => event.post_balance.to_le_bytes()[index - 82],
        90..=97 => event.slot.to_le_bytes()[index - 90],
        98..=105 => event.timestamp.to_le_bytes()[index - 98],
        _ => unreachable!(),
    }
}

#[cfg(target_os = "solana")]
pub fn emit_transfer_event(event: &NorthstarTransferEvent) {
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
    log_transfer_event_data(event);
}
