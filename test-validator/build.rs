use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct BundledProgram {
    name: &'static str,
    manifest: &'static str,
    binary: &'static str,
    target_subdir: &'static str,
}

const BUNDLED_PROGRAMS: &[BundledProgram] = &[
    BundledProgram {
        name: "portal",
        manifest: "northstar/programs/portal/Cargo.toml",
        binary: "northstar_portal.so",
        target_subdir: "portal-sbf-target",
    },
    BundledProgram {
        name: "token bridge",
        manifest: "northstar/programs/token-bridge/Cargo.toml",
        binary: "northstar_token_bridge.so",
        target_subdir: "token-bridge-sbf-target",
    },
];

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = find_workspace_root(&manifest_dir);
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("Cargo.lock").display()
    );
    for program in BUNDLED_PROGRAMS {
        let manifest = workspace_root.join(program.manifest);
        println!(
            "cargo:rerun-if-changed={}",
            manifest.parent().unwrap().display()
        );
    }

    if running_under_clippy() {
        for program in BUNDLED_PROGRAMS {
            fs::write(out_dir.join(program.binary), []).unwrap();
        }
        return;
    }

    for program in BUNDLED_PROGRAMS {
        let manifest = workspace_root.join(program.manifest);
        let target_dir = out_dir.join(program.target_subdir);
        let output = run_cargo_build_sbf(&manifest, &out_dir, &target_dir, false);
        if !output.status.success() {
            emit_command_output(&output);
            if should_retry_with_force_tools_install(&output) {
                let retry = run_cargo_build_sbf(&manifest, &out_dir, &target_dir, true);
                if !retry.status.success() {
                    emit_command_output(&retry);
                    panic!(
                        "`cargo build-sbf --force-tools-install` for {} failed with status {}",
                        program.name, retry.status
                    );
                }
            } else {
                panic!(
                    "`cargo build-sbf` for {} failed with status {}",
                    program.name, output.status
                );
            }
        }

        let binary = out_dir.join(program.binary);
        if !binary.is_file() {
            panic!(
                "{} program binary missing at {}; `cargo build-sbf --manifest-path {}` should \
                 have produced it",
                program.name,
                binary.display(),
                manifest.display()
            );
        }
    }
}

fn run_cargo_build_sbf(
    manifest: &Path,
    out_dir: &Path,
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
        .arg(out_dir)
        .arg("--")
        .arg("--target-dir")
        .arg(target_dir);
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

fn running_under_clippy() -> bool {
    ["RUSTC_WORKSPACE_WRAPPER", "RUSTC_WRAPPER", "RUSTC"]
        .iter()
        .filter_map(env::var_os)
        .any(|value| value.to_string_lossy().contains("clippy"))
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
