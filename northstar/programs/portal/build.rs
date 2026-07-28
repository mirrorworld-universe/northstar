use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=NORTHSTAR_ALLOW_TEST_VERIFIER_SBF");
    println!("cargo:rerun-if-changed=Cargo.toml");

    if env::var_os("NORTHSTAR_ALLOW_TEST_VERIFIER_SBF").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        println!("cargo:rustc-cfg=northstar_allow_test_verifier_sbf");
    }
}
