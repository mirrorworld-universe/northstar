#![cfg(unix)]

mod support;

use {
    northstar_vrf_oracle::{Attempt, Error, Oracle, Status, verify},
    serde::{Deserialize, Serialize},
    std::{
        env, fs,
        io::{BufRead, BufReader, Read, Write},
        os::unix::{fs::MetadataExt, process::ExitStatusExt},
        path::Path,
        process::{Child, Command, ExitStatus, Stdio},
        sync::mpsc::{self, Receiver},
        thread::{self, JoinHandle},
        time::Duration,
    },
    support::{key, metadata, policy, request},
};

const SENTINEL: &str = "VRF_JOURNAL_TEST_READY=";
const ROLE: &str = "NORTHSTAR_VRF_TEST_PROCESS_ROLE";
const DIRECTORY: &str = "NORTHSTAR_VRF_TEST_DIRECTORY";
const EXPECTED: &str = "NORTHSTAR_VRF_TEST_EXPECTED_PENDING";

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
enum Report {
    Locked,
    Pending {
        attempt: Attempt,
        attempts: u64,
        status: Status,
    },
}

fn observation(oracle: &Oracle, directory: &Path) -> Report {
    let request = request();
    let snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("public-state.json")).unwrap()).unwrap();
    let attempt = oracle.pending(request.id).unwrap().unwrap();
    verify(&metadata(), &request, &attempt.result).unwrap();
    Report::Pending {
        attempt,
        attempts: snapshot["records"][hex::encode(request.id)]["attempts"]
            .as_u64()
            .unwrap(),
        status: oracle.status(request.id).unwrap(),
    }
}

fn emit(report: &Report) {
    let mut stdout = std::io::stdout().lock();
    writeln!(
        stdout,
        "\n{SENTINEL}{}",
        serde_json::to_string(report).unwrap()
    )
    .unwrap();
    stdout.flush().unwrap();
}

#[test]
#[ignore = "subprocess entry used by the process recovery tests"]
fn subprocess_journal_helper() {
    let directory = env::var_os(DIRECTORY).unwrap();
    let directory = Path::new(&directory);
    assert!(
        directory.is_dir(),
        "the parent must pre-provision the directory"
    );
    match env::var(ROLE).unwrap().as_str() {
        "hold" => {
            let mut oracle = Oracle::open(directory, metadata(), policy()).unwrap();
            let request = request();
            oracle.ingest(request.clone(), 0).unwrap();
            let first = oracle.prepare(request.id, 10, &key()).unwrap().unwrap();
            let second = oracle.prepare(request.id, 15, &key()).unwrap().unwrap();
            assert_eq!(first.result, second.result);
            assert_ne!(first.attempt_id, second.attempt_id);
            // prepare has returned after persistence. Keep the lock alive while
            // blocked on the parent's pipe; the parent terminates us with SIGKILL.
            emit(&observation(&oracle, directory));
            std::io::stdin().read_exact(&mut [0]).unwrap();
            drop(oracle);
            panic!("the parent must terminate this child, not release it cleanly");
        }
        "contend" => {
            assert!(matches!(
                Oracle::open(directory, metadata(), policy()),
                Err(Error::Locked)
            ));
            emit(&Report::Locked);
        }
        "recover" => {
            let expected: Report = serde_json::from_str(&env::var(EXPECTED).unwrap()).unwrap();
            let mut oracle = Oracle::open(directory, metadata(), policy()).unwrap();
            assert_eq!(observation(&oracle, directory), expected);
            for tick in [15, 16, 19] {
                assert!(
                    oracle
                        .prepare(request().id, tick, &key())
                        .unwrap()
                        .is_none()
                );
                assert_eq!(observation(&oracle, directory), expected);
            }
            emit(&observation(&oracle, directory));
        }
        role => panic!("unknown test helper role: {role}"),
    }
}

struct Helper {
    child: Child,
    reports: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl Helper {
    fn spawn(role: &str, directory: &Path, expected: Option<&Report>) -> Self {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "subprocess_journal_helper",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ROLE, role)
            .env(DIRECTORY, directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(expected) = expected {
            command.env(EXPECTED, serde_json::to_string(expected).unwrap());
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, reports) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(report) = line.strip_prefix(SENTINEL) {
                    let _ = sender.send(report.to_owned());
                }
            }
        });
        Self {
            child,
            reports,
            reader: Some(reader),
        }
    }

    fn report(&self) -> Report {
        let message = self
            .reports
            .recv_timeout(Duration::from_secs(30))
            .expect("child did not signal its known boundary");
        serde_json::from_str(&message).unwrap()
    }

    fn wait(&mut self) -> ExitStatus {
        self.child.wait().unwrap()
    }

    fn terminate(&mut self) {
        assert!(self.child.try_wait().unwrap().is_none());
        self.child.kill().unwrap();
        assert_eq!(self.wait().signal(), Some(9));
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn assert_second_pending(report: &Report) {
    match report {
        Report::Pending {
            attempts, status, ..
        } => {
            assert_eq!(*attempts, 2);
            assert_eq!(
                *status,
                Status::Pending {
                    submitted_at: 15,
                    retry_at: 20
                }
            );
        }
        Report::Locked => panic!("expected a durable Pending intent, not a lock failure"),
    }
}

#[test]
fn second_process_cannot_open_while_writer_owns_lock() {
    let directory = tempfile::tempdir().unwrap();
    let mut writer = Helper::spawn("hold", directory.path(), None);
    assert_second_pending(&writer.report());
    let snapshot = fs::read(directory.path().join("public-state.json")).unwrap();
    assert!(writer.child.try_wait().unwrap().is_none());
    let mut contender = Helper::spawn("contend", directory.path(), None);
    assert_eq!(contender.report(), Report::Locked);
    assert!(contender.wait().success());
    assert!(writer.child.try_wait().unwrap().is_none());
    assert_eq!(
        fs::read(directory.path().join("public-state.json")).unwrap(),
        snapshot
    );
    writer.terminate();
}

#[test]
fn terminated_writer_recovers_exact_pending_without_early_resend_or_confirmation() {
    let directory = tempfile::tempdir().unwrap();
    let mut writer = Helper::spawn("hold", directory.path(), None);
    let expected = writer.report();
    assert_second_pending(&expected);
    let lock_path = directory.path().join("writer.lock");
    let lock_before = fs::metadata(&lock_path).unwrap();
    let snapshot = fs::read(directory.path().join("public-state.json")).unwrap();
    writer.terminate();
    assert_eq!(
        fs::read(directory.path().join("public-state.json")).unwrap(),
        snapshot
    );
    let mut recovered = Helper::spawn("recover", directory.path(), Some(&expected));
    assert_eq!(recovered.report(), expected);
    assert!(recovered.wait().success());
    let lock_after = fs::metadata(lock_path).unwrap();
    assert_eq!(
        (lock_after.dev(), lock_after.ino()),
        (lock_before.dev(), lock_before.ino())
    );
}
