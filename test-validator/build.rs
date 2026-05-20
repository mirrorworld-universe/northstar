use std::{
    env,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Output},
    thread::sleep,
    time::{Duration, SystemTime},
};

const BUILD_SBF_GUARD: &str = "NORTHSTAR_PORTAL_BUILD_SBF_RUNNING";
const PROGRAM_SO: &str = "northstar_portal.so";
const SBF_BUILD_LOCK: &str = "northstar_portal_sbf_build.lock";
const STALE_LOCK_TIMEOUT: Duration = Duration::from_secs(20 * 60);

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = find_workspace_root(&manifest_dir);
    let portal_manifest = workspace_root.join("northstar/programs/portal/Cargo.toml");
    let portal_src = workspace_root.join("northstar/programs/portal/src");
    let sbf_out_dir = env::var_os("BPF_OUT_DIR")
        .or_else(|| env::var_os("SBF_OUT_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target/deploy"));
    let sbf_out_dir = absolute_path(sbf_out_dir);
    let program_so = sbf_out_dir.join(PROGRAM_SO);

    println!("cargo:rerun-if-env-changed=BPF_OUT_DIR");
    println!("cargo:rerun-if-env-changed=SBF_OUT_DIR");
    println!("cargo:rerun-if-changed={}", portal_manifest.display());
    println!("cargo:rerun-if-changed={}", portal_src.display());
    println!("cargo:rerun-if-changed={}", program_so.display());
    println!("cargo:rustc-check-cfg=cfg(northstar_skip_portal_program_binary)");
    println!(
        "cargo:rustc-env=NORTHSTAR_PORTAL_PROGRAM_SO={}",
        program_so.display()
    );

    if running_under_clippy() {
        println!("cargo:rustc-cfg=northstar_skip_portal_program_binary");
        return;
    }

    build_portal_if_needed(&portal_manifest, &portal_src, &sbf_out_dir, &program_so);

    if !program_so.exists() {
        panic!(
            "portal program binary missing at {}; `cargo build-sbf --manifest-path {}` should \
             have produced it",
            program_so.display(),
            portal_manifest.display()
        );
    }
}

fn build_portal_if_needed(
    portal_manifest: &Path,
    portal_src: &Path,
    sbf_out_dir: &Path,
    program_so: &Path,
) {
    fs::create_dir_all(sbf_out_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create SBF output directory {}: {err}",
            sbf_out_dir.display()
        )
    });

    let _lock = SbfBuildLock::acquire(sbf_out_dir);
    if !needs_portal_build(portal_manifest, portal_src, program_so) {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target_dir = out_dir.join("portal-sbf-target");
    let output = run_cargo_build_sbf(portal_manifest, sbf_out_dir, &target_dir, false);
    if !output.status.success() {
        emit_command_output(&output);
        if should_retry_with_force_tools_install(&output) {
            eprintln!(
                "`cargo build-sbf` failed with status {}; retrying with `--force-tools-install` \
                 in case cached Solana platform tools are corrupt",
                output.status
            );
            let retry_output = run_cargo_build_sbf(portal_manifest, sbf_out_dir, &target_dir, true);
            if !retry_output.status.success() {
                emit_command_output(&retry_output);
                panic!(
                    "`cargo build-sbf --force-tools-install` failed with status {}",
                    retry_output.status
                );
            }
        } else {
            panic!("`cargo build-sbf` failed with status {}", output.status);
        }
    }
}

fn run_cargo_build_sbf(
    portal_manifest: &Path,
    sbf_out_dir: &Path,
    target_dir: &Path,
    force_tools_install: bool,
) -> Output {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.arg("build-sbf");
    if force_tools_install {
        command.arg("--force-tools-install");
    }
    command
        .arg("--manifest-path")
        .arg(portal_manifest)
        .arg("--sbf-out-dir")
        .arg(sbf_out_dir)
        .arg("--")
        .arg("--target-dir")
        .arg(target_dir)
        .env(BUILD_SBF_GUARD, "1");
    remove_cargo_driver_env(&mut command);

    command
        .output()
        .unwrap_or_else(|err| panic!("failed to run `cargo build-sbf`: {err}"))
}

fn emit_command_output(output: &Output) {
    eprint!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
}

fn should_retry_with_force_tools_install(output: &Output) -> bool {
    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));

    text.contains("--force-tools-install")
        || (text.contains("platform-tools")
            && (text.contains("not a directory")
                || text.contains("No such file")
                || text.contains("corrupt")))
}

fn needs_portal_build(portal_manifest: &Path, portal_src: &Path, program_so: &Path) -> bool {
    let Ok(program_mtime) = mtime(program_so) else {
        return true;
    };

    [portal_manifest, portal_src]
        .iter()
        .any(|path| newest_mtime(path).is_some_and(|input_mtime| input_mtime > program_mtime))
}

fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.is_dir() {
        let mut newest = metadata.modified().ok();
        for entry in fs::read_dir(path).ok()? {
            let entry = entry.ok()?;
            if let Some(entry_mtime) = newest_mtime(&entry.path()) {
                newest = Some(newest.map_or(entry_mtime, |mtime| mtime.max(entry_mtime)));
            }
        }
        newest
    } else {
        metadata.modified().ok()
    }
}

fn mtime(path: &Path) -> std::io::Result<SystemTime> {
    fs::metadata(path)?.modified()
}

fn absolute_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .unwrap_or_else(|err| panic!("failed to determine current directory: {err}"))
            .join(path)
    }
}

fn running_under_clippy() -> bool {
    ["RUSTC_WORKSPACE_WRAPPER", "RUSTC_WRAPPER", "RUSTC"]
        .iter()
        .filter_map(env::var_os)
        .any(|value| value.to_string_lossy().contains("clippy"))
}

struct SbfBuildLock {
    path: PathBuf,
}

impl SbfBuildLock {
    fn acquire(sbf_out_dir: &Path) -> Self {
        let path = sbf_out_dir.join(SBF_BUILD_LOCK);
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Self { path },
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        let _ = fs::remove_file(&path);
                    } else {
                        sleep(Duration::from_millis(250));
                    }
                }
                Err(err) => panic!("failed to acquire SBF build lock {}: {err}", path.display()),
            }
        }
    }
}

impl Drop for SbfBuildLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_is_stale(path: &Path) -> bool {
    mtime(path)
        .and_then(|mtime| mtime.elapsed().map_err(std::io::Error::other))
        .is_ok_and(|elapsed| elapsed > STALE_LOCK_TIMEOUT)
}

fn remove_cargo_driver_env(command: &mut Command) {
    for (key, _) in env::vars_os() {
        let key = key.to_string_lossy();
        if key.contains("RUSTFLAGS") || key.contains("RUSTC") || key.contains("RUSTDOC") {
            command.env_remove(key.as_ref());
        }
    }
}

fn find_workspace_root(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .find(|path| path.join("Cargo.lock").is_file() && path.join("Cargo.toml").is_file())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.to_path_buf())
}
