fn main() {
    println!("cargo:rerun-if-env-changed=NORTHSTAR_VRF_PROFILE_SHA256");
    if std::env::var_os("CARGO_FEATURE_CANDIDATE").is_none() {
        return;
    }
    let value = std::env::var("NORTHSTAR_VRF_PROFILE_SHA256")
        .expect("candidate guest requires a reviewed build-time NORTHSTAR_VRF_PROFILE_SHA256");
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "canonical SHA-256 hex"
    );
    let digest: Vec<_> = value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    assert!(
        digest.iter().any(|byte| *byte != 0),
        "zero candidate policy is not reviewable"
    );
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("policy.rs");
    std::fs::write(
        output,
        format!("pub const APPROVED_PROFILE_SHA256: [u8; 32] = {digest:?};\n"),
    )
    .unwrap();
}
