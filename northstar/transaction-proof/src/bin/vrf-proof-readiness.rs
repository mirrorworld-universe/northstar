use {
    northstar_transaction_proof::readiness::{
        inspect_encoded, ExpectedBindings, MAX_READINESS_WITNESS_BYTES,
    },
    std::{env, fs::File, io::Read, process::ExitCode},
};

fn bounded_read(path: &str, limit: usize) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|error| format!("{path}: {error}"))?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{path}: {error}"))?;
    if bytes.len() > limit {
        return Err(format!("{path}: exceeds {limit}-byte local input limit"));
    }
    Ok(bytes)
}

fn hash_arg(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64 || !value.is_ascii() {
        return Err("anchor must be exactly 64 hexadecimal characters".into());
    }
    let mut hash = [0; 32];
    for (byte, digits) in hash.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
        let digits = std::str::from_utf8(digits).map_err(|error| error.to_string())?;
        *byte = u8::from_str_radix(digits, 16)
            .map_err(|_| "anchor must be exactly 64 hexadecimal characters".to_string())?;
    }
    Ok(hash)
}

fn run() -> Result<ExitCode, String> {
    let mut args = env::args().skip(1);
    let usage = "usage: vrf-proof-readiness WITNESS PUBLIC_INPUTS [--checkpoint-root HEX32] \
                 [--runtime-profile HEX32] [--require-anchors] [--require-vrf]";
    let witness_path = args.next().ok_or(usage)?;
    let public_path = args.next().ok_or(usage)?;
    let mut checkpoint_root = None;
    let mut runtime_profile = None;
    let mut require_anchors = false;
    let mut require_vrf = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--checkpoint-root" if checkpoint_root.is_none() => {
                checkpoint_root = Some(hash_arg(&args.next().ok_or(usage)?)?);
            }
            "--runtime-profile" if runtime_profile.is_none() => {
                runtime_profile = Some(hash_arg(&args.next().ok_or(usage)?)?);
            }
            "--require-anchors" if !require_anchors => require_anchors = true,
            "--require-vrf" if !require_vrf => require_vrf = true,
            _ => return Err(usage.into()),
        }
    }
    let encoded = bounded_read(&witness_path, MAX_READINESS_WITNESS_BYTES)?;
    let public = bounded_read(&public_path, 256)?;
    let report = inspect_encoded(
        &encoded,
        ExpectedBindings {
            public_inputs: &public,
            checkpoint_transaction_effect_root: checkpoint_root,
            runtime_profile_sha256: runtime_profile,
        },
    )
    .map_err(|error| format!("witness encoding rejected: {error:?}"))?;
    print!("{}", report.to_text());
    Ok(if !report.conformant() {
        ExitCode::from(2)
    } else if require_anchors && !report.all_bindings_anchored() {
        eprintln!("--require-anchors requires both --checkpoint-root and --runtime-profile pins");
        ExitCode::from(4)
    } else if require_vrf {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    })
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_encoding_is_exact() {
        assert_eq!(hash_arg(&"aB".repeat(32)), Ok([0xab; 32]));
        for value in [
            "0".repeat(63),
            "0".repeat(65),
            "zz".repeat(32),
            "é".repeat(32),
        ] {
            assert!(hash_arg(&value).is_err());
        }
    }
}
