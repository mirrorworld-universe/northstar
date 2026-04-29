use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::SystemTime,
};

const BUILD_SBF_GUARD: &str = "NORTHSTAR_PORTAL_BUILD_SBF_RUNNING";
const PROGRAM_SO: &str = "northstar_portal.so";

fn main() {
    println!("cargo:rerun-if-env-changed=BPF_OUT_DIR");
    println!("cargo:rerun-if-env-changed=SBF_OUT_DIR");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = find_workspace_root(&manifest_dir);
    let sbf_out_dir = env::var_os("BPF_OUT_DIR")
        .or_else(|| env::var_os("SBF_OUT_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target/deploy"));
    let sbf_out_dir = absolute_path(sbf_out_dir);

    println!("cargo:rustc-env=BPF_OUT_DIR={}", sbf_out_dir.display());
    println!("cargo:rustc-env=SBF_OUT_DIR={}", sbf_out_dir.display());

    if env::var_os(BUILD_SBF_GUARD).is_some() || is_sbf_target() {
        return;
    }

    let program_so = sbf_out_dir.join(PROGRAM_SO);
    if !needs_sbf_rebuild(&manifest_dir, &program_so) {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let nested_target_dir = out_dir.join("sbf-target");
    let nested_sbf_out_dir = out_dir.join("sbf-deploy");
    fs::create_dir_all(&nested_sbf_out_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create nested SBF output directory {}: {err}",
            nested_sbf_out_dir.display()
        )
    });

    let output = run_cargo_build_sbf(
        &manifest_dir,
        &nested_sbf_out_dir,
        &nested_target_dir,
        false,
    );
    if !output.status.success() {
        emit_command_output(&output);
        if should_retry_with_force_tools_install(&output) {
            eprintln!(
                "`cargo build-sbf` failed with status {}; retrying with `--force-tools-install` \
                 in case cached Solana platform tools are corrupt",
                output.status
            );
            let retry_output =
                run_cargo_build_sbf(&manifest_dir, &nested_sbf_out_dir, &nested_target_dir, true);
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

    let nested_program_so = nested_sbf_out_dir.join(PROGRAM_SO);
    if !nested_program_so.exists() {
        panic!(
            "`cargo build-sbf` succeeded but {} was not produced",
            nested_program_so.display()
        );
    }

    fs::create_dir_all(&sbf_out_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create SBF output directory {}: {err}",
            sbf_out_dir.display()
        )
    });
    fs::copy(&nested_program_so, &program_so).unwrap_or_else(|err| {
        panic!(
            "failed to copy {} to {}: {err}",
            nested_program_so.display(),
            program_so.display()
        )
    });
}

fn run_cargo_build_sbf(
    manifest_dir: &Path,
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
        .arg(manifest_dir.join("Cargo.toml"))
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

fn absolute_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .unwrap_or_else(|err| panic!("failed to determine current directory: {err}"))
            .join(path)
    }
}

fn is_sbf_target() -> bool {
    env::var("TARGET").is_ok_and(|target| target.contains("sbf"))
        || env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|arch| arch == "sbf")
}

fn needs_sbf_rebuild(manifest_dir: &Path, program_so: &Path) -> bool {
    let Ok(program_mtime) = mtime(program_so) else {
        return true;
    };

    [manifest_dir.join("Cargo.toml"), manifest_dir.join("src")]
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
