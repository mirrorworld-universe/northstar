use {
    anyhow::{Result, ensure},
    std::{
        fs::OpenOptions,
        io::Read,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
        path::Path,
    },
    zeroize::Zeroizing,
};
pub(crate) fn read_regular(path: &Path, maximum: u64, private: bool) -> Result<Zeroizing<Vec<u8>>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "input must be a regular file");
    ensure!(
        !private || metadata.permissions().mode() & 0o077 == 0,
        "credential must be private"
    );
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= maximum, "input exceeds bounds");
    Ok(bytes)
}
pub(crate) fn bearer(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = read_regular(path, 512, true)?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    ensure!(
        (32..=256).contains(&bytes.len()) && bytes.iter().all(|b| b.is_ascii_graphic()),
        "invalid bearer credential"
    );
    Ok(bytes)
}
