use std::{
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

fn allowed_environment(key: &OsStr) -> bool {
    matches!(
        key.to_str(),
        Some(
            "PATH"
                | "HOME"
                | "LD_LIBRARY_PATH"
                | "RUST_LOG"
                | "SSL_CERT_FILE"
                | "NIX_SSL_CERT_FILE"
                | "HTTP_PROXY"
                | "HTTPS_PROXY"
                | "NO_PROXY"
        )
    ) || key
        .to_str()
        .is_some_and(|key| key.starts_with("NORTHSTAR_GPU_") || key.starts_with("CUDA_"))
}

struct Process {
    child: Child,
    reaped: bool,
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // The unreaped child owns this process-group ID, so it cannot have been reused.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

pub fn run(
    executable: &Path,
    arguments: &[OsString],
    directory: &Path,
    timeout: Duration,
    stop: &AtomicBool,
) -> io::Result<Duration> {
    let output = directory.join("process.log");
    let log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)?;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .current_dir(directory)
        .env_clear()
        .envs(std::env::vars_os().filter(|(key, _)| allowed_environment(key)))
        .env("SP1_PROVER", "cuda")
        .env("NORTHSTAR_GPU_REQUEST_TIMEOUT", "120")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0);
    #[cfg(target_os = "linux")]
    {
        let parent = std::process::id() as i32;
        // Only direct syscalls and an OS error value are used between fork and exec.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ECANCELED));
                }
                Ok(())
            });
        }
    }
    let started = Instant::now();
    let mut process = Process {
        child: command.spawn()?,
        reaped: false,
    };
    loop {
        if stop.load(Ordering::Relaxed) || started.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "proof process interrupted or expired",
            ));
        }
        if fs::metadata(&output)?.len() > 4 * 1024 * 1024 {
            return Err(io::Error::other("proof process log limit"));
        }
        if let Some(status) = process.child.try_wait()? {
            // try_wait reaped the child; never signal a potentially reused process-group ID.
            process.reaped = true;
            return if status.success() {
                Ok(started.elapsed())
            } else {
                Err(io::Error::other(format!("proof process exited {status}")))
            };
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_configuration_is_not_inherited() {
        for key in [
            "NORTHSTAR_PROOF_CHALLENGER_KEYPAIR",
            "NORTHSTAR_LIVE_PAYER",
            "TRANSFER_SOURCE_PRIVATE_KEY",
            "SP1_PRIVATE_KEY",
        ] {
            assert!(!allowed_environment(OsStr::new(key)));
        }
        assert!(allowed_environment(OsStr::new(
            "NORTHSTAR_GPU_WORKER_SOCKET"
        )));
    }

    #[test]
    fn timeout_reaps_child_and_success_is_reported() {
        let root = tempfile::tempdir().unwrap();
        let shell = Path::new("/bin/sh");
        let elapsed = Instant::now();
        let error = run(
            shell,
            &["-c".into(), "sleep 30".into()],
            root.path(),
            Duration::from_millis(100),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(elapsed.elapsed() < Duration::from_secs(5));
        let next = tempfile::tempdir().unwrap();
        assert!(run(
            shell,
            &["-c".into(), "exit 0".into()],
            next.path(),
            Duration::from_secs(2),
            &AtomicBool::new(false)
        )
        .is_ok());
    }
}
