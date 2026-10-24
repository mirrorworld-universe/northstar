use {
    northstar_vrf_sbpf_replay::sidecar,
    std::{env, path::Path, process::ExitCode},
};
fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        [command, capture, session, output] if command == "produce" => {
            sidecar::produce(Path::new(capture), Path::new(session), Path::new(output))
        }
        [binding, session] => sidecar::validate(Path::new(binding), Path::new(session)),
        _ => Err(
            "usage: vrf-candidate-binding produce CAPTURE_JSON SESSION_JSON FRESH_OUTPUT_DIR | \
             BINDING_JSON SESSION_JSON"
                .into(),
        ),
    }
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
