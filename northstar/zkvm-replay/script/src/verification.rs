use {
    anyhow::{anyhow, bail, Result},
    serde_json::json,
    std::{fs, time::Instant},
};

pub fn benchmark(mut args: impl Iterator<Item = String>) -> Result<()> {
    let proof_path = args
        .next()
        .ok_or_else(|| anyhow!("missing on-chain proof path"))?;
    let public_path = args
        .next()
        .ok_or_else(|| anyhow!("missing public inputs path"))?;
    let output_path = args
        .next()
        .ok_or_else(|| anyhow!("missing measurement path"))?;
    let repetitions = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(10);
    if !(1..=1000).contains(&repetitions) || args.next().is_some() {
        bail!("usage: verify PROOF PUBLIC_INPUTS MEASUREMENTS [repetitions: 1..=1000]");
    }
    if std::path::Path::new(&output_path).exists() {
        bail!("refusing to overwrite verification measurements");
    }
    let proof = fs::read(&proof_path)?;
    let public = fs::read(&public_path)?;
    if proof.len() != 356 || public.len() != 256 {
        bail!("expected a 356-byte SP1 envelope and 256-byte public inputs");
    }
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../partial-candidate-v1.json"))?;
    let program_key = manifest["program_vkey_hash"]
        .as_str()
        .ok_or_else(|| anyhow!("missing candidate key"))?;
    let verify = |proof: &[u8], public: &[u8]| {
        sp1_verifier::Groth16Verifier::verify(
            proof,
            public,
            program_key,
            &sp1_verifier::GROTH16_VK_BYTES,
        )
        .map_err(|error| anyhow!("host verification failed: {error:?}"))
    };
    // Loading artifacts and parsing the manifest are deliberately outside the timer.
    let started = Instant::now();
    verify(&proof, &public)?;
    let first_verify_us = started.elapsed().as_micros();
    let mut samples = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let started = Instant::now();
        verify(&proof, &public)?;
        samples.push(started.elapsed().as_micros());
    }
    let mut changed_public = public.clone();
    changed_public[255] ^= 1;
    if verify(&proof, &changed_public).is_ok() {
        bail!("changed public inputs were accepted");
    }
    let mut changed_proof = proof.clone();
    changed_proof[355] ^= 1;
    if verify(&changed_proof, &public).is_ok() {
        bail!("changed proof was accepted");
    }
    let mut sorted = samples.clone();
    sorted.sort_unstable();
    let output = json!({
        "schema": "northstar-host-verification-v1",
        "verifier": "sp1-verifier-6.8.0",
        "program_vkey_hash": program_key,
        "proof_hex": hex::encode(&proof),
        "public_inputs_hex": hex::encode(&public),
        "first_verify_us": first_verify_us,
        "steady_verify_us": samples,
        "median_verify_us": sorted[sorted.len() / 2],
        "changed_public_inputs_rejected": true,
        "changed_proof_rejected": true,
        "includes_proving": false,
        "includes_artifact_io": false,
    });
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)?;
    serde_json::to_writer_pretty(&mut file, &output)?;
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_retained_proof_and_refuses_measurement_overwrite() {
        let evidence =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../evidence/sp1-v6.8.0");
        let output = std::env::temp_dir().join(format!(
            "northstar-host-verification-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let args = vec![
            evidence
                .join("northstar-sp1-groth16-onchain.bin")
                .to_str()
                .unwrap()
                .to_owned(),
            evidence
                .join("northstar-sp1-public-inputs.bin")
                .to_str()
                .unwrap()
                .to_owned(),
            output.to_str().unwrap().to_owned(),
            "2".to_owned(),
        ];
        benchmark(args.clone().into_iter()).unwrap();
        let before = fs::read(&output).unwrap();
        let measurement: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(measurement["steady_verify_us"].as_array().unwrap().len(), 2);
        assert_eq!(measurement["changed_proof_rejected"], true);
        assert!(benchmark(args.into_iter()).is_err());
        assert_eq!(fs::read(&output).unwrap(), before);
        fs::remove_file(output).unwrap();
    }

    #[test]
    fn rejects_unbounded_repetitions_before_reading_artifacts() {
        for repetitions in ["0", "1001", "invalid"] {
            let error = benchmark(
                [
                    "missing-proof",
                    "missing-public",
                    "missing-output",
                    repetitions,
                ]
                .into_iter()
                .map(str::to_owned),
            )
            .unwrap_err()
            .to_string();
            if repetitions == "invalid" {
                assert!(error.contains("invalid digit"), "{error}");
            } else {
                assert!(error.contains("repetitions: 1..=1000"), "{error}");
            }
        }
    }
}
