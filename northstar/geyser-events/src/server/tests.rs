use {
    super::*,
    crate::{
        config::Config,
        event::{ProgramEvent, Source},
    },
    agave_geyser_plugin_interface::geyser_plugin_interface::SlotStatus,
    solana_pubkey::Pubkey,
    tokio::io::{AsyncReadExt, AsyncWriteExt},
};

async fn request(addr: SocketAddr, path: &str, headers: &str) -> tokio::net::TcpStream {
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    socket
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{headers}\r\n").as_bytes())
        .await
        .unwrap();
    socket
}

async fn read_until(socket: &mut tokio::net::TcpStream, needle: &str) -> String {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut output = String::new();
        loop {
            let mut buffer = [0; 4096];
            let length = socket.read(&mut buffer).await.unwrap();
            assert!(length > 0, "connection closed before {needle}: {output}");
            output.push_str(std::str::from_utf8(&buffer[..length]).unwrap());
            if output.contains(needle) {
                return output;
            }
        }
    })
    .await
    .expect("HTTP/SSE response timeout")
}

fn emit(broker: &Broker, key: Pubkey, slot: u64) {
    broker.transaction(
        slot,
        slot,
        0,
        vec![ProgramEvent {
            program_id: key.to_string(),
            slot,
            block_time: None,
            signature: "signature".into(),
            tx_index: 0,
            log_index: 0,
            source: Source::Log,
            commitment: crate::config::Commitment::Confirmed,
            data: "AAAAAAAAAAA=".into(),
        }],
    );
    broker.metadata(slot, slot, slot - 1, 1, Some(123));
    broker.status(slot, slot, Some(slot - 1), &SlotStatus::Confirmed);
}

#[tokio::test]
async fn http_stream_replays_and_unload_closes_clients() {
    let broker = Broker::new(Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        ..Config::default()
    });
    let mut server = Server::start(broker.clone()).unwrap();
    let key = Pubkey::new_unique();
    let path = format!("/v1/events/{key}");
    let mut socket = request(server.addr, &path, "").await;
    let headers = read_until(&mut socket, "\r\n\r\n").await;
    assert!(headers.contains("200 OK"));
    assert!(headers.contains("text/event-stream"));
    emit(&broker, key, 10);
    let data = read_until(&mut socket, "\n\n").await;
    assert!(data.contains("id: 10:0:0\nevent: program_event"));
    assert!(data.contains("\"block_time\":123"));
    emit(&broker, key, 11);
    let mut resumed = request(server.addr, &path, "Last-Event-ID: 10:0:0\r\n").await;
    let data = read_until(&mut resumed, "id: 11:0:0").await;
    assert!(!data.contains("id: 10:0:0"));
    assert!(!data.contains("resync_required"));
    server.stop();
    let mut buffer = [0; 4096];
    tokio::time::timeout(Duration::from_secs(3), async {
        while resumed.read(&mut buffer).await.unwrap() != 0 {}
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn invalid_requests_are_rejected_and_health_is_available() {
    let broker = Broker::new(Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        ..Config::default()
    });
    let server = Server::start(broker).unwrap();
    let key = Pubkey::new_unique();
    for (path, headers) in [
        ("/v1/events/not-a-pubkey".to_string(), ""),
        (format!("/v1/events/{key}?from_slot=-1"), ""),
        (format!("/v1/events/{key}?from_slot=1&from_slot=2"), ""),
        (format!("/v1/events/{key}?commitment=processed"), ""),
        (format!("/v1/events/{key}"), "Last-Event-ID: invalid\r\n"),
    ] {
        let mut socket = request(server.addr, &path, headers).await;
        assert!(
            read_until(&mut socket, "\r\n\r\n")
                .await
                .contains("400 Bad Request")
        );
    }
    let mut socket = request(server.addr, "/health", "").await;
    assert!(read_until(&mut socket, "\r\n\r\n").await.contains("200 OK"));
}

#[tokio::test(start_paused = true)]
async fn heartbeat_and_slow_consumer_signal_are_explicit() {
    let broker = Broker::new(Config {
        live_capacity: 1,
        ..Config::default()
    });
    let key = Pubkey::new_unique();
    let subscription = broker.subscribe(key, Resume::default()).unwrap();
    let heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(15),
        Duration::from_secs(15),
    );
    let mut stream = Stream {
        subscription,
        heartbeat,
        finished: false,
    };
    assert_eq!(stream.next().await.unwrap(), ": ping\n\n");
    emit(&broker, key, 10);
    emit(&broker, key, 11);
    let notice = stream.next().await.unwrap();
    assert!(notice.contains("event: resync_required"));
    assert!(notice.contains("slow_consumer"));
    assert!(stream.next().await.is_none());
}

#[test]
fn last_event_id_takes_precedence_and_ids_are_strict() {
    let resume = parse_resume(Some("from_slot=4"), Some(Ok("10:2:3"))).unwrap();
    assert_eq!(resume.after.unwrap().to_string(), "10:2:3");
    assert!(resume.from_slot.is_none());
    for value in [
        "",
        "1:2",
        "1:2:3:4",
        "-1:0:0",
        "+1:0:0",
        "1: 0:0",
        "18446744073709551616:0:0",
    ] {
        assert!(value.parse::<EventId>().is_err(), "{value}");
    }
    assert!(serde_json::from_str::<Config>(r#"{"commitment":"processed"}"#).is_err());
    assert!(
        Config {
            live_capacity: 0,
            ..Config::default()
        }
        .validate()
        .is_err()
    );
}
