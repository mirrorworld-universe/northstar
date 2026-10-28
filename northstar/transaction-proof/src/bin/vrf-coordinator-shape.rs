#[cfg(all(feature = "host", not(target_os = "zkvm")))]
fn main() -> std::process::ExitCode {
    use {
        northstar_transaction_proof::vrf_coordinator_profile::{
            inspect_signed_transaction, MAX_DIAGNOSTIC_TRANSACTION_BYTES,
        },
        std::{fs::File, io::Read, process::ExitCode},
    };
    let mut args = std::env::args_os().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: vrf-coordinator-shape SIGNED_LEGACY_TRANSACTION.bin");
        return ExitCode::from(2);
    };
    if args.next().is_some() {
        eprintln!("expected exactly one transaction path");
        return ExitCode::from(2);
    }
    let result = (|| {
        let mut bytes = Vec::new();
        File::open(path)?
            .take((MAX_DIAGNOSTIC_TRANSACTION_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        inspect_signed_transaction(&bytes)
            .map_err(|error| std::io::Error::other(format!("{error:?}")))
    })();
    match result {
        Ok(report) => {
            print!("{}", report.to_text());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("diagnostic rejected input: {error}");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(all(feature = "host", not(target_os = "zkvm"))))]
fn main() {
    eprintln!("vrf-coordinator-shape requires a native host build with feature host");
    std::process::exit(2);
}
