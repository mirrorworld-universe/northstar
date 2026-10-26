use {
    anyhow::{anyhow, bail, Context, Result},
    serde::Deserialize,
    serde_json::json,
    sha2::{Digest, Sha256},
    sp1_sdk::{
        include_elf,
        network::{signer::NetworkSigner, FulfillmentStrategy, B256},
        Elf, HashableKey, ProveRequest, Prover, ProverClient, ProvingKey, SP1ProofWithPublicValues,
        SP1Stdin,
    },
    std::{
        env, fs,
        io::{Read, Write},
        os::unix::fs::OpenOptionsExt,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    },
};

#[path = "../network_state.rs"]
mod state;

const ELF: Elf = include_elf!("northstar-zkvm-replay-program");
const PROOF_FILE: &str = "northstar-sp1-groth16.bin";
const ONCHAIN_FILE: &str = "northstar-sp1-groth16-onchain.bin";
const PUBLIC_FILE: &str = "northstar-sp1-public-inputs.bin";

#[derive(Deserialize)]
struct Manifest {
    elf_sha256: String,
    baseline_fixture_sha256: String,
    program_vkey_hash: String,
}

struct Config {
    key_file: PathBuf,
    state_directory: PathBuf,
    max_gas: u64,
    max_cycles: u64,
    max_price_per_pgu: u64,
}

impl Config {
    fn load(hosted: bool) -> Result<Self> {
        let required = |name| env::var(name).with_context(|| format!("{name} is required"));
        Ok(Self {
            key_file: required("NORTHSTAR_NETWORK_PRIVATE_KEY_FILE")?.into(),
            state_directory: required("NORTHSTAR_NETWORK_STATE_DIR")?.into(),
            max_gas: state::positive_limit(
                "NORTHSTAR_NETWORK_MAX_GAS",
                &required("NORTHSTAR_NETWORK_MAX_GAS")?,
                u64::MAX,
            )?,
            max_cycles: state::positive_limit(
                "NORTHSTAR_NETWORK_MAX_CYCLES",
                &required("NORTHSTAR_NETWORK_MAX_CYCLES")?,
                u64::MAX,
            )?,
            // SP1 6.8.0 multiplies this value by 10^9 when logging the price per bPGU.
            max_price_per_pgu: state::positive_limit(
                "NORTHSTAR_NETWORK_MAX_PRICE_PER_PGU",
                &if hosted {
                    "1".to_string()
                } else {
                    required("NORTHSTAR_NETWORK_MAX_PRICE_PER_PGU")?
                },
                u64::MAX / 1_000_000_000,
            )?,
        })
    }
}

fn write_new(path: impl AsRef<Path>, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn check_simulation(
    public: &[u8],
    expected: &[u8; 256],
    cycles: u64,
    gas: Option<u64>,
    config: &Config,
) -> Result<()> {
    if public != expected {
        bail!("SP1 execute public values differ from canonical inputs");
    }
    if cycles > config.max_cycles
        || gas.context("SP1 simulation did not report gas")? > config.max_gas
    {
        bail!("network simulation exceeds configured cycle or gas limit");
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn native_inputs(encoded: &[u8]) -> Result<[u8; 256]> {
    let witness = northstar_zkvm_replay_shared::decode_witness(encoded)
        .map_err(|error| anyhow!("decode witness: {error:?}"))?;
    Ok(northstar_zkvm_replay_shared::public_inputs_bytes(
        northstar_zkvm_replay_shared::replay(&witness)
            .map_err(|error| anyhow!("native replay: {error:?}"))?,
    ))
}

fn check_proof(proof: &SP1ProofWithPublicValues, expected: &[u8; 256]) -> Result<Vec<u8>> {
    if proof.public_values.as_slice() != expected {
        bail!("network proof public values differ from canonical inputs");
    }
    if !matches!(&proof.proof, sp1_sdk::SP1Proof::Groth16(_)) {
        bail!("network proof is not Groth16");
    }
    let bytes = proof.bytes();
    if bytes.len() != 356 || bytes[..4] != [0x43, 0x88, 0xa2, 0x1c] {
        bail!("network proof envelope mismatch");
    }
    Ok(bytes)
}

#[tokio::main]
async fn main() -> Result<()> {
    tokio::time::timeout(Duration::from_millis(state::DEADLINE_MS), run())
        .await
        .context("network adapter deadline expired; remote work may still be billable")?
}

async fn run() -> Result<()> {
    let command_started_ms = state::now_ms()?;
    let args: Vec<_> = env::args().skip(1).collect();
    let preflight = args == ["preflight"];
    if !preflight && (args.len() != 4 || args[0] != "groth16") {
        bail!("usage: northstar-network-prover preflight | groth16 WITNESS MEASUREMENTS PROFILE");
    }
    let (rpc_url, hosted) = state::endpoint(
        &env::var("NORTHSTAR_NETWORK_MODE").unwrap_or_else(|_| "auction".into()),
        env::var("NORTHSTAR_NETWORK_RPC_URL").ok().as_deref(),
    )?;
    if !preflight && !hosted && env::var("NORTHSTAR_NETWORK_ALLOW_PAID").as_deref() != Ok("1") {
        bail!("set NORTHSTAR_NETWORK_ALLOW_PAID=1 to authorize witness upload and paid proving");
    }
    let config = Config::load(hosted)?;
    state::private_directory(&config.state_directory)?;
    let manifest: Manifest =
        serde_json::from_str(include_str!("../../../partial-candidate-v1.json"))?;
    if digest(ELF.as_ref()) != manifest.elf_sha256 {
        bail!("embedded network guest ELF differs from frozen candidate");
    }
    let encoded = if preflight {
        let fixture = include_bytes!("../../../fixture-v1.bin").to_vec();
        if digest(&fixture) != manifest.baseline_fixture_sha256 {
            bail!("baseline fixture hash mismatch");
        }
        fixture
    } else {
        let mut encoded = Vec::new();
        let file = fs::File::open(&args[1])?;
        if !file.metadata()?.is_file() {
            bail!("witness must be a regular file");
        }
        file.take(4 * 1024 * 1024 + 1).read_to_end(&mut encoded)?;
        if encoded.len() > 4 * 1024 * 1024 {
            bail!("network witness exceeds 4 MiB");
        }
        for path in [&args[2], PROOF_FILE, ONCHAIN_FILE, PUBLIC_FILE] {
            if fs::symlink_metadata(path).is_ok() {
                bail!("refusing to overwrite prior proof artifacts");
            }
        }
        encoded
    };
    let expected = native_inputs(&encoded)?;
    let signer = NetworkSigner::local(&state::read_private_key(&config.key_file)?)
        .map_err(|_| anyhow!("invalid network signing key"))?;
    let requester = signer.address().to_string();
    let started = Instant::now();
    // Explicit client selection ignores SP1_PROVER, including the legacy CUDA runner setting.
    let mut builder = ProverClient::builder()
        .network()
        .rpc_url(&rpc_url)
        .signer(signer);
    if hosted {
        builder = builder.hosted();
    }
    let client = builder.build().await;
    let client_init_ms = started.elapsed().as_millis();
    let key = client.setup(ELF).await?;
    let program_vkey_hash = key.verifying_key().bytes32();
    if program_vkey_hash != manifest.program_vkey_hash {
        bail!("embedded network program key mismatch");
    }
    let mut stdin = SP1Stdin::new();
    stdin.write_vec(encoded.clone());
    if preflight {
        let (public, report) = client.execute(ELF, stdin).calculate_gas(true).await?;
        check_simulation(
            public.as_slice(),
            &expected,
            report.total_instruction_count(),
            report.gas(),
            &config,
        )?;
        let balance = client.get_balance().await?;
        if !hosted && balance.is_zero() {
            bail!("network requester has no PROVE balance");
        }
        if !hosted {
            client
                .get_proof_request_params(sp1_sdk::SP1ProofMode::Groth16)
                .await?;
        }
        println!(
            "{}",
            json!({"backend": "succinct-network", "network_mode": if hosted { "local-hosted" } else { "auction" }, "program_vkey_hash": program_vkey_hash, "requester": requester, "paid_request_submitted": false})
        );
        return Ok(());
    }

    let witness_sha256 = digest(&encoded);
    let cache_id =
        digest(format!("{rpc_url}:{requester}:{program_vkey_hash}:{witness_sha256}").as_bytes());
    let journal = state::Journal::open(config.state_directory.join(cache_id))?;
    let mut record = match journal.load()? {
        Some(record) => record,
        None => {
            let (public, report) = client
                .execute(ELF, stdin.clone())
                .calculate_gas(true)
                .await?;
            check_simulation(
                public.as_slice(),
                &expected,
                report.total_instruction_count(),
                report.gas(),
                &config,
            )?;
            let record = state::Request {
                witness_sha256: witness_sha256.clone(),
                program_vkey_hash: program_vkey_hash.clone(),
                requester: requester.clone(),
                started_ms: command_started_ms,
                request_id: None,
                completed_ms: None,
            };
            let remaining = state::remaining(record.started_ms, state::now_ms()?)?;
            // Persist intent first. An uncertain submission must never trigger another paid request.
            journal.store(&record)?;
            let request_id = client
                .prove(&key, stdin)
                .groth16()
                .strategy(if hosted {
                    FulfillmentStrategy::Reserved
                } else {
                    FulfillmentStrategy::Auction
                })
                .timeout(remaining)
                .cycle_limit(config.max_cycles)
                .gas_limit(config.max_gas)
                .max_price_per_pgu(config.max_price_per_pgu)
                .private_stdin(true)
                .request()
                .await?;
            let mut record = record;
            record.request_id = Some(request_id.to_string());
            journal.store(&record)?;
            record
        }
    };
    record.validate(&witness_sha256, &program_vkey_hash, &requester)?;
    let request_id: B256 = record.submitted_id()?.parse()?;
    let cached_proof = journal.directory.join("proof.bin");
    let proof = if record.completed_ms.is_some() {
        SP1ProofWithPublicValues::load(&cached_proof)?
    } else {
        let remaining = state::remaining(record.started_ms, state::now_ms()?)?;
        client
            .wait_proof(request_id, Some(remaining), Some(Duration::from_secs(30)))
            .await?
    };
    let proof_bytes = check_proof(&proof, &expected)?;
    let started = Instant::now();
    client.verify(&proof, key.verifying_key(), None)?;
    let verify_ms = started.elapsed().as_millis();
    if record.completed_ms.is_none() {
        let completed = state::now_ms()?;
        state::remaining(record.started_ms, completed)?;
        proof.save(&cached_proof)?;
        fs::File::open(&cached_proof)?.sync_all()?;
        record.completed_ms = Some(completed);
        journal.store(&record)?;
    }
    let prove_and_wrap_ms = record
        .completed_ms
        .context("network proof completion missing")?
        - record.started_ms;
    write_new(PROOF_FILE, &fs::read(&cached_proof)?)?;
    write_new(ONCHAIN_FILE, &proof_bytes)?;
    write_new(PUBLIC_FILE, &expected)?;
    let measurements = json!({
        "schema": "northstar-sp1-benchmark-v2",
        "backend": "succinct-network",
        "profile": args[3],
        "program_vkey_hash": program_vkey_hash,
        "public_inputs": hex::encode(expected),
        "witness_bytes": encoded.len(),
        "witness_sha256": witness_sha256,
        "client_init_ms": client_init_ms,
        "network_request_id": request_id.to_string(),
        "network_rpc_url": rpc_url,
        "network_mode": if hosted { "local-hosted" } else { "auction" },
        "sp1_groth16_vkey_sha256": "4388a21c687fdd5f218d7e3d13190cac4c5355818d3605fd5fb811df468ee696",
        "sp1_verifier_root": hex::encode(*sp1_verifier::VK_ROOT_BYTES),
        "phases": [{
            "phase": "groth16", "prove_and_wrap_ms": prove_and_wrap_ms, "verify_ms": verify_ms,
            "onchain_proof_bytes": proof_bytes.len(), "onchain_proof_path": ONCHAIN_FILE,
            "public_inputs_path": PUBLIC_FILE, "artifact_bytes": fs::metadata(PROOF_FILE)?.len(),
        }],
    });
    write_new(&args[2], &serde_json::to_vec_pretty(&measurements)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_guest_and_fixture_match_manifest() {
        let manifest: Manifest =
            serde_json::from_str(include_str!("../../../partial-candidate-v1.json")).unwrap();
        assert_eq!(digest(ELF.as_ref()), manifest.elf_sha256);
        let encoded = include_bytes!("../../../fixture-v1.bin");
        assert_eq!(digest(encoded), manifest.baseline_fixture_sha256);
        native_inputs(encoded).unwrap();
    }

    #[test]
    fn artifacts_are_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("artifact");
        write_new(&path, b"original").unwrap();
        assert!(write_new(&path, b"changed").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
    }

    #[test]
    fn simulation_limits_and_outputs_are_checked() {
        let config = Config {
            key_file: PathBuf::new(),
            state_directory: PathBuf::new(),
            max_gas: 100,
            max_cycles: 1000,
            max_price_per_pgu: 1,
        };
        let expected = [0; 256];
        check_simulation(&expected, &expected, 1000, Some(100), &config).unwrap();
        assert!(check_simulation(&expected, &expected, 1001, Some(100), &config).is_err());
        assert!(check_simulation(&expected, &expected, 1000, Some(101), &config).is_err());
        assert!(check_simulation(&expected, &expected, 1000, None, &config).is_err());
        assert!(check_simulation(&[1; 256], &expected, 1000, Some(100), &config).is_err());
    }

    #[tokio::test]
    async fn frozen_key_and_execution_are_valid_without_cuda_or_network() {
        let client = ProverClient::builder().light().build().await;
        let key = client.setup(ELF).await.unwrap();
        let manifest: Manifest =
            serde_json::from_str(include_str!("../../../partial-candidate-v1.json")).unwrap();
        assert_eq!(key.verifying_key().bytes32(), manifest.program_vkey_hash);
        let encoded = include_bytes!("../../../fixture-v1.bin");
        let mut stdin = SP1Stdin::new();
        stdin.write_vec(encoded.to_vec());
        let (public, report) = client
            .execute(ELF, stdin)
            .calculate_gas(true)
            .await
            .unwrap();
        assert_eq!(public.as_slice(), native_inputs(encoded).unwrap());
        assert!(report.gas().unwrap() > 0);
    }

    fn check_retained_case(case: &str) {
        let proof = SP1ProofWithPublicValues::load(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../evidence")
                .join(case)
                .join("northstar-sp1-groth16.bin"),
        )
        .unwrap();
        let expected = native_inputs(include_bytes!("../../../fixture-v1.bin")).unwrap();
        let bytes = check_proof(&proof, &expected).unwrap();
        assert_eq!(bytes.len(), 356);
        let manifest: Manifest =
            serde_json::from_str(include_str!("../../../partial-candidate-v1.json")).unwrap();
        let verify = |bytes: &[u8], public: &[u8]| {
            sp1_verifier::Groth16Verifier::verify(
                bytes,
                public,
                &manifest.program_vkey_hash,
                &sp1_verifier::GROTH16_VK_BYTES,
            )
        };
        verify(&bytes, &expected).unwrap();
        let mut changed_proof = bytes.clone();
        changed_proof[355] ^= 1;
        assert!(verify(&changed_proof, &expected).is_err());
        let mut changed = expected;
        changed[32] ^= 1;
        assert!(check_proof(&proof, &changed).is_err());
        assert!(verify(&bytes, &changed).is_err());
    }

    #[test]
    fn retained_proof_envelope_and_public_inputs_are_checked() {
        check_retained_case("g5-a10g-sha-v1");
    }

    #[test]
    fn retained_network_proof_and_changed_inputs_are_checked() {
        check_retained_case("succinct-network-local-v1");
    }
}
