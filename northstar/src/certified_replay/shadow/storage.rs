use {
    super::{hash_parts, invalid, MAX_IMAGE},
    std::{
        fs::{self, DirBuilder, File, OpenOptions},
        io::{self, Read, Write},
        os::{
            fd::AsRawFd,
            unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        },
        path::{Component, Path, PathBuf},
    },
};

const NAME: &str = "replay-v2";
fn directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}
fn held(dir: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()))
}
fn private(dir: &File, uid: u32) -> io::Result<()> {
    let m = dir.metadata()?;
    if m.uid() != uid || m.mode() & 0o7777 != 0o700 {
        return Err(invalid("shadow directory must be owned and mode 0700"));
    }
    Ok(())
}

pub(super) struct Store(File);
impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
impl Store {
    pub fn open(path: &Path, create: bool) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("missing shadow parent"))?;
        let mut dir = directory(if parent.is_absolute() {
            Path::new("/")
        } else {
            Path::new(".")
        })?;
        for c in parent.components() {
            match c {
                Component::RootDir | Component::CurDir => (),
                Component::Normal(name) => dir = directory(&held(&dir).join(name))?,
                _ => return Err(invalid("invalid shadow directory component")),
            }
        }
        let uid = fs::metadata("/proc/self")?.uid();
        private(&dir, uid)?;
        let child = held(&dir).join(
            path.file_name()
                .ok_or_else(|| invalid("missing shadow directory name"))?,
        );
        if create {
            DirBuilder::new().mode(0o700).create(&child)?;
            dir.sync_all()?;
        }
        let store = directory(&child)?;
        private(&store, uid)?;
        store.try_lock().map_err(io::Error::other)?;
        Ok(Self(store))
    }
    pub fn read(&self) -> io::Result<Vec<u8>> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(held(&self.0).join(NAME))?;
        let meta = file.metadata()?;
        let max = MAX_IMAGE as u64 + 32;
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != self.0.metadata()?.uid()
            || meta.mode() & 0o7777 != 0o600
            || meta.len() > max
        {
            return Err(invalid(
                "shadow store requires a bounded private regular file",
            ));
        }
        let mut bytes = Vec::new();
        file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != meta.len() {
            return Err(invalid("shadow file length changed"));
        }
        let (image, digest) = bytes
            .split_last_chunk::<32>()
            .ok_or_else(|| invalid("incomplete shadow snapshot"))?;
        if hash_parts(&[image]) != *digest {
            return Err(invalid("shadow image checksum differs"));
        }
        bytes.truncate(bytes.len().saturating_sub(32));
        Ok(bytes)
    }
    pub fn save(&self, image: &[u8]) -> io::Result<()> {
        if image.len() > MAX_IMAGE {
            return Err(invalid("shadow snapshot exceeds bound"));
        }
        let path = held(&self.0);
        let mut tmp = tempfile::NamedTempFile::new_in(&path)?;
        tmp.write_all(image)?;
        tmp.write_all(&hash_parts(&[image]))?;
        tmp.as_file().sync_all()?;
        tmp.persist(path.join(NAME)).map_err(|e| e.error)?;
        self.0.sync_all()
    }
}
