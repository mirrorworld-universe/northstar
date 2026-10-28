use std::process::{Command, Output};

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_northstar-vrf-oracle"))
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn published_vector_runner_and_public_only_verifier() {
    for command in ["vector", "verify-vector"] {
        let output = run(&[command]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            !stdout.contains("3431c2b03533e280b23232e280b34e2c3132c2b03238e280b23131e280b34500")
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn separate_process_restart_recovers_pending_intent_and_confirms_only_explicitly() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    for (tick, action, expected) in [
        ("10", "prepare", "public intent, NOT network submission"),
        ("11", "prepare", "Pending"),
        ("15", "prepare", "public intent, NOT network submission"),
        ("16", "confirm", "Confirmed"),
        ("17", "prepare", "Confirmed"),
    ] {
        let output = run(&["demo", path, tick, action]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(expected), "{stdout}");
        if tick == "11" || tick == "17" {
            assert!(!stdout.contains("public intent, NOT network submission"));
        }
    }
    assert!(!run(&["demo", path, "15"]).status.success());
}

#[test]
fn bad_fixture_action_cannot_create_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unused");
    assert!(
        !run(&["demo", path.to_str().unwrap(), "10", "live"])
            .status
            .success()
    );
    assert!(!path.exists());
}
