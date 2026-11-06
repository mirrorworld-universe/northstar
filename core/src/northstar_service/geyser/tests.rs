// Sonic: Exercises the actual dynamic plugin through ER history and HTTP SSE.
use {
    super::*,
    solana_message::{Message, VersionedMessage},
    solana_pubkey::Pubkey,
    solana_rpc::er_history::ErHistoryStore,
    solana_runtime::genesis_utils::create_genesis_config,
    solana_signature::Signature,
    solana_transaction::versioned::VersionedTransaction,
    solana_transaction_status::TransactionStatusMeta,
    std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        time::Duration,
    },
};

fn read_until(socket: &mut TcpStream, needle: &str) -> String {
    let mut result = String::new();
    while !result.contains(needle) {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).expect("event HTTP response");
        assert!(count > 0, "SSE closed before {needle}: {result}");
        result.push_str(std::str::from_utf8(&buffer[..count]).unwrap());
    }
    result
}

#[test]
#[ignore = "build northstar-geyser-events and set NORTHSTAR_EVENTS_PLUGIN_LIB to its shared library"]
fn test_er_history_dynamic_geyser_sse_roundtrip() {
    let library = std::fs::canonicalize(
        std::env::var_os("NORTHSTAR_EVENTS_PLUGIN_LIB").expect("plugin library path"),
    )
    .unwrap();
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("events.json");
    std::fs::write(
        &config,
        serde_json::to_vec(
            &serde_json::json!({ "libpath": library, "listen_addr": address.to_string() }),
        )
        .unwrap(),
    )
    .unwrap();
    let observer = ErGeyserObserver::load(&[config]).unwrap().unwrap();
    let store = ErHistoryStore::default();
    store.set_observer(observer);
    let bank = Bank::new_for_tests(&create_genesis_config(1_000_000).genesis_config);
    let program = Pubkey::new_unique();
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        socket,
        "GET /v1/events/{program} HTTP/1.1\r\nHost: localhost\r\n\r\n"
    )
    .unwrap();
    assert!(read_until(&mut socket, "\r\n\r\n").contains("200 OK"));
    for index in 0..2u8 {
        let transaction = VersionedTransactionWithStatusMeta {
            transaction: VersionedTransaction {
                signatures: vec![Signature::from([index + 1; 64])],
                message: VersionedMessage::Legacy(Message {
                    account_keys: vec![program],
                    ..Message::default()
                }),
            },
            meta: TransactionStatusMeta {
                log_messages: Some(vec![
                    format!("Program {program} invoke [1]"),
                    "Program data: AAAAAAAAAAA=".into(),
                    format!("Program {program} success"),
                ]),
                ..TransactionStatusMeta::default()
            },
        };
        assert_eq!(
            store.record_transaction(&bank, transaction),
            Some(u32::from(index))
        );
    }
    socket.set_nonblocking(true).unwrap();
    assert_eq!(
        socket.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    socket.set_nonblocking(false).unwrap();
    bank.freeze();
    store.finalize_slot(&bank);
    let output = read_until(&mut socket, "id: 0:1:0");
    assert!(output.contains("id: 0:0:0"));
    assert!(output.contains("\"commitment\":\"confirmed\""));
    drop(store);
    let mut remaining = Vec::new();
    socket.read_to_end(&mut remaining).unwrap();
}
