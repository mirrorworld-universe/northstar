use {
    northstar_vrf_oracle::{Metadata, Mode, Oracle, OracleKey, Policy, Request, SUITE, VrfResult},
    serde::Deserialize,
    std::{env, error::Error},
    zeroize::Zeroizing,
};

#[derive(Deserialize)]
struct PublicVector {
    public_test_scalar: String,
    public_key: String,
    alpha: String,
    proof: String,
    output: String,
}

fn bytes32(value: &str) -> Result<[u8; 32], Box<dyn Error>> {
    Ok(hex::decode(value)?
        .try_into()
        .map_err(|_| "expected 32 bytes")?)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let vector: PublicVector = serde_json::from_str(include_str!("../tests/fixtures/c2sp.json"))?;
    match args.first().map(String::as_str) {
        Some("verify-vector") if args.len() == 1 => {
            let result = VrfResult {
                proof: vector.proof,
                output: vector.output,
            };
            result.verify_bytes(bytes32(&vector.public_key)?, &hex::decode(vector.alpha)?)?;
            println!("published C2SP proof verified with public key only");
        }
        Some("vector") if args.len() == 1 => {
            let key = OracleKey::from_scalar(Zeroizing::new(bytes32(&vector.public_test_scalar)?))?;
            let result = key.prove_bytes(&hex::decode(vector.alpha)?)?;
            if result.proof != vector.proof || result.output != vector.output {
                return Err("published vector mismatch".into());
            }
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Some("demo") if (3..=4).contains(&args.len()) => {
            let now: u64 = args[2].parse()?;
            let action = args.get(3).map(String::as_str).unwrap_or("prepare");
            if !["prepare", "confirm", "reject"].contains(&action) {
                return Err("unknown fixture action".into());
            }
            let metadata = Metadata {
                version: 1,
                mode: Mode::OfflineFixtureV1,
                suite: SUITE.into(),
                chain: [1; 32],
                program: [2; 32],
                application_fixture_digest: [3; 32],
                consumer_fixture_digest: [4; 32],
            };
            let request = Request {
                metadata_digest: metadata.digest()?,
                id: [5; 32],
                public_key: bytes32(&vector.public_key)?,
                input: b"PUBLIC offline request; no on-chain ABI".to_vec(),
                context_digest: [6; 32],
                not_before: 10,
                expires_at: 1000,
            };
            let mut oracle = Oracle::open(
                &args[1],
                metadata,
                Policy {
                    max_attempts: 3,
                    retry_delay: 2,
                    pending_timeout: 5,
                },
            )?;
            let ingest = oracle.ingest(request.clone(), now)?;
            println!("offline fixture: {ingest:?}");
            match action {
                "prepare" => {
                    let key = OracleKey::from_scalar(Zeroizing::new(bytes32(
                        &vector.public_test_scalar,
                    )?))?;
                    if let Some(attempt) = oracle.prepare(request.id, now, &key)? {
                        println!(
                            "public intent, NOT network submission: {}",
                            serde_json::to_string(&attempt)?
                        );
                    }
                }
                "confirm" | "reject" => {
                    let attempt = oracle
                        .pending(request.id)?
                        .ok_or("no pending fixture intent")?;
                    if action == "confirm" {
                        oracle.confirm_fixture(&attempt, now)?;
                    } else {
                        oracle.rejected_fixture(&attempt, now)?;
                    }
                }
                _ => unreachable!(),
            }
            println!("{}", serde_json::to_string(&oracle.status(request.id)?)?);
        }
        _ => {
            return Err("usage: northstar-vrf-oracle vector | verify-vector | demo \
                        STATE_DIRECTORY TICK [prepare|confirm|reject]"
                .into());
        }
    }
    Ok(())
}
