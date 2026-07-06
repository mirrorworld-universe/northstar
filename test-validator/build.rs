use std::{
    env,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Output},
    thread::sleep,
    time::{Duration, SystemTime},
};

const BUILD_SBF_GUARD: &str = "NORTHSTAR_TEST_VALIDATOR_BUILD_SBF_RUNNING";
const SBF_BUILD_LOCK: &str = "northstar_test_validator_sbf_build.lock";
const STALE_LOCK_TIMEOUT: Duration = Duration::from_secs(20 * 60);

struct BundledProgram {
    name: &'static str,
    manifest: &'static str,
    src: &'static str,
    so: &'static str,
    env: &'static str,
    skip_cfg: &'static str,
    target_subdir: &'static str,
}

const BUNDLED_PROGRAMS: &[BundledProgram] = &[
    BundledProgram {
        name: "portal",
        manifest: "northstar/programs/portal/Cargo.toml",
        src: "northstar/programs/portal/src",
        so: "northstar_portal.so",
        env: "NORTHSTAR_PORTAL_PROGRAM_SO",
        skip_cfg: "northstar_skip_portal_program_binary",
        target_subdir: "portal-sbf-target",
    },
    BundledProgram {
        name: "token bridge",
        manifest: "northstar/programs/token-bridge/Cargo.toml",
        src: "northstar/programs/token-bridge/src",
        so: "northstar_token_bridge.so",
        env: "NORTHSTAR_TOKEN_BRIDGE_PROGRAM_SO",
        skip_cfg: "northstar_skip_token_bridge_program_binary",
        target_subdir: "token-bridge-sbf-target",
    },
];

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = find_workspace_root(&manifest_dir);
    let sbf_out_dir = env::var_os("BPF_OUT_DIR")
        .or_else(|| env::var_os("SBF_OUT_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target/deploy"));
    let sbf_out_dir = absolute_path(sbf_out_dir);

    println!("cargo:rerun-if-env-changed=BPF_OUT_DIR");
    println!("cargo:rerun-if-env-changed=SBF_OUT_DIR");

    for program in BUNDLED_PROGRAMS {
        let manifest = workspace_root.join(program.manifest);
        let src = workspace_root.join(program.src);
        let program_so = sbf_out_dir.join(program.so);

        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rerun-if-changed={}", src.display());
        println!("cargo:rerun-if-changed={}", program_so.display());
        println!("cargo:rustc-check-cfg=cfg({})", program.skip_cfg);
        println!("cargo:rustc-env={}={}", program.env, program_so.display());
    }

    if running_under_clippy() {
        for program in BUNDLED_PROGRAMS {
            println!("cargo:rustc-cfg={}", program.skip_cfg);
        }
        return;
    }

    fs::create_dir_all(&sbf_out_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create SBF output directory {}: {err}",
            sbf_out_dir.display()
        )
    });

    let _lock = SbfBuildLock::acquire(&sbf_out_dir);
    for program in BUNDLED_PROGRAMS {
        let manifest = workspace_root.join(program.manifest);
        let src = workspace_root.join(program.src);
        let program_so = sbf_out_dir.join(program.so);

        build_program_if_needed(program, &manifest, &src, &sbf_out_dir, &program_so);

        if !program_so.exists() {
            panic!(
                "{} program binary missing at {}; `cargo build-sbf --manifest-path {}` should \
                 have produced it",
                program.name,
                program_so.display(),
                manifest.display()
            );
        }
    }
}

fn build_program_if_needed(
    program: &BundledProgram,
    manifest: &Path,
    src: &Path,
    sbf_out_dir: &Path,
    program_so: &Path,
) {
    if !needs_program_build(manifest, src, program_so) {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target_dir = out_dir.join(program.target_subdir);
    let output = run_cargo_build_sbf(manifest, sbf_out_dir, &target_dir, false);
    if !output.status.success() {
        emit_command_output(&output);
        if should_retry_with_force_tools_install(&output) {
            eprintln!(
                "`cargo build-sbf` for {} failed with status {}; retrying with \
                 `--force-tools-install` in case cached Solana platform tools are corrupt",
                program.name, output.status
            );
            let retry_output = run_cargo_build_sbf(manifest, sbf_out_dir, &target_dir, true);
            if !retry_output.status.success() {
                emit_command_output(&retry_output);
                panic!(
                    "`cargo build-sbf --force-tools-install` for {} failed with status {}",
                    program.name, retry_output.status
                );
            }
        } else {
            panic!(
                "`cargo build-sbf` for {} failed with status {}",
                program.name, output.status
            );
        }
    }
}

fn run_cargo_build_sbf(
    manifest: &Path,
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
        .arg(manifest)
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

fn needs_program_build(manifest: &Path, src: &Path, program_so: &Path) -> bool {
    let Ok(program_mtime) = mtime(program_so) else {
        return true;
    };

    [manifest, src]
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
