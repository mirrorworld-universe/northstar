use std::{env, fs, io::Write, process::ExitCode};

fn digest(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("canonical profile SHA-256 required".into());
    }
    let mut output = [0; 32];
    for (pair, byte) in value.as_bytes().as_chunks::<2>().0.iter().zip(&mut output) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).map_err(|e| e.to_string())?, 16)
            .map_err(|e| e.to_string())?;
    }
    Ok(output)
}
fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    let [witness, profile, output] = args.as_slice() else {
        return Err(
            "usage: northstar-zkvm-vrf-script WITNESS PROFILE_SHA256 FRESH_PUBLIC_OUTPUT (native \
             replay only; use zkvm-vrf-client for SP1/GPU)"
                .into(),
        );
    };
    let bytes = fs::read(witness).map_err(|e| e.to_string())?;
    let public = northstar_zkvm_vrf_shared::replay(&bytes, digest(profile)?)
        .map_err(|e| format!("actual native replay: {e:?}"))?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| e.to_string())?
        .write_all(&public)
        .map_err(|e| e.to_string())?;
    println!("native execution/binding only; 256 public bytes; no generated proof");
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
