use {
    anyhow::{Result, bail, ensure},
    solana_keypair::Keypair,
    solana_signer::Signer,
    std::{
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        path::Path,
    },
    zeroize::Zeroizing,
};
pub fn load(path: &Path) -> Result<Keypair> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "key file must be regular and private (0600)"
    );
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(4097).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4096, "key file exceeds limit");
    let decoded: Vec<u8> = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid development key JSON"))?;
    let decoded = Zeroizing::new(decoded);
    Keypair::try_from(decoded.as_slice())
        .map_err(|_| anyhow::anyhow!("invalid development keypair"))
}
pub fn generate(directory: &Path) -> Result<()> {
    if directory.exists() {
        bail!("key directory already exists; refusing to overwrite development keys");
    }
    fs::DirBuilder::new().mode(0o700).create(directory)?;
    let mut public = serde_json::Map::new();
    for name in ["signer", "vrf"] {
        let key = Keypair::new();
        let bytes = Zeroizing::new(key.to_bytes());
        let json = Zeroizing::new(serde_json::to_vec(bytes.as_slice())?);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join(format!("{name}.json")))?;
        file.write_all(&json)?;
        file.sync_all()?;
        public.insert(name.to_owned(), key.pubkey().to_string().into());
    }
    File::open(directory)?.sync_all()?;
    println!("{}", serde_json::Value::Object(public));
    Ok(())
}
