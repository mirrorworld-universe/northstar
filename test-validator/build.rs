use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

const BUILD_SBF_GUARD: &str = "NORTHSTAR_PORTAL_BUILD_SBF_RUNNING";
const PROGRAM_SO: &str = "northstar_portal.so";
const SBF_TARGET_DIR: &str = "northstar-portal-sbf";

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = find_workspace_root(&manifest_dir);
    let portal_manifest = workspace_root.join("northstar/programs/portal/Cargo.toml");
    let sbf_out_dir = workspace_root.join("target/deploy");
    let sbf_target_dir = workspace_root.join("target").join(SBF_TARGET_DIR);
    let program_so = sbf_out_dir.join(PROGRAM_SO);

    println!("cargo:rerun-if-changed={}", portal_manifest.display());
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root
            .join("northstar/programs/portal/src")
            .display()
    );
    println!(
        "cargo:rustc-env=NORTHSTAR_PORTAL_PROGRAM_SO={}",
        program_so.display()
    );

    if program_so.exists() {
        return;
    }

    std::fs::create_dir_all(&sbf_out_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create SBF output directory {}: {err}",
            sbf_out_dir.display()
        )
    });

    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .arg("build-sbf")
        .arg("--manifest-path")
        .arg(&portal_manifest)
        .arg("--sbf-out-dir")
        .arg(&sbf_out_dir)
        .arg("--")
        .arg("--target-dir")
        .arg(&sbf_target_dir)
        .env(BUILD_SBF_GUARD, "1");
    remove_cargo_driver_env(&mut command);

    let status = command
        .status()
        .unwrap_or_else(|err| panic!("failed to run `cargo build-sbf`: {err}"));

    if !status.success() {
        panic!("`cargo build-sbf` failed with status {status}");
    }

    if !program_so.exists() {
        panic!(
            "`cargo build-sbf` succeeded but {} was not produced",
            program_so.display()
        );
    }
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
