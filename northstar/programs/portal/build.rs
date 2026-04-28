use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
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

    if env::var_os(BUILD_SBF_GUARD).is_some() {
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
        .arg(manifest_dir.join("Cargo.toml"))
        .arg("--sbf-out-dir")
        .arg(&sbf_out_dir)
        .env(BUILD_SBF_GUARD, "1");
    remove_cargo_driver_env(&mut command);

    let status = command
        .status()
        .unwrap_or_else(|err| panic!("failed to run `cargo build-sbf`: {err}"));

    if !status.success() {
        panic!("`cargo build-sbf` failed with status {status}");
    }

    let program_so = sbf_out_dir.join(PROGRAM_SO);
    if !program_so.exists() {
        panic!(
            "`cargo build-sbf` succeeded but {} was not produced",
            program_so.display()
        );
    }
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
