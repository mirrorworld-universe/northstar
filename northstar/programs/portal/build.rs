use std::{
    env,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rerun-if-env-changed=BPF_OUT_DIR");
    println!("cargo:rerun-if-env-changed=SBF_OUT_DIR");
    println!("cargo:rerun-if-env-changed=NORTHSTAR_ALLOW_TEST_VERIFIER_SBF");
    println!("cargo:rerun-if-changed=Cargo.toml");

    if env::var_os("NORTHSTAR_ALLOW_TEST_VERIFIER_SBF").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        println!("cargo:rustc-cfg=northstar_allow_test_verifier_sbf");
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = find_workspace_root(&manifest_dir);
    let sbf_out_dir = env::var_os("BPF_OUT_DIR")
        .or_else(|| env::var_os("SBF_OUT_DIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target/deploy"));
    let sbf_out_dir = absolute_path(sbf_out_dir);

    println!("cargo:rustc-env=BPF_OUT_DIR={}", sbf_out_dir.display());
    println!("cargo:rustc-env=SBF_OUT_DIR={}", sbf_out_dir.display());
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
