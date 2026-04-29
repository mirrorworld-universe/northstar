use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

const PROGRAM_SO: &str = "northstar_portal.so";

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = find_workspace_root(&manifest_dir);
    let portal_manifest = workspace_root.join("northstar/programs/portal/Cargo.toml");
    let portal_src = workspace_root.join("northstar/programs/portal/src");
    let sbf_out_dir = env::var_os("BPF_OUT_DIR")
        .or_else(|| env::var_os("SBF_OUT_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target/deploy"));
    let program_so = absolute_path(sbf_out_dir).join(PROGRAM_SO);

    println!("cargo:rerun-if-env-changed=BPF_OUT_DIR");
    println!("cargo:rerun-if-env-changed=SBF_OUT_DIR");
    println!("cargo:rerun-if-changed={}", portal_manifest.display());
    println!("cargo:rerun-if-changed={}", portal_src.display());
    println!("cargo:rerun-if-changed={}", program_so.display());
    println!(
        "cargo:rustc-env=NORTHSTAR_PORTAL_PROGRAM_SO={}",
        program_so.display()
    );

    if needs_portal_build(&portal_manifest, &portal_src, &program_so) {
        build_portal(&portal_manifest);
    }

    if !program_so.exists() {
        panic!(
            "portal program binary missing at {}; northstar-portal build.rs should have produced \
             it",
            program_so.display()
        );
    }
}

fn build_portal(portal_manifest: &Path) {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let target_dir = out_dir.join("portal-check-target");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .arg("check")
        .arg("--manifest-path")
        .arg(portal_manifest)
        .arg("--target-dir")
        .arg(target_dir)
        .status()
        .unwrap_or_else(|err| panic!("failed to run `cargo check` for northstar-portal: {err}"));

    if !status.success() {
        panic!("`cargo check` for northstar-portal failed with status {status}");
    }
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

fn find_workspace_root(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .ancestors()
        .find(|path| path.join("Cargo.lock").is_file() && path.join("Cargo.toml").is_file())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.to_path_buf())
}
