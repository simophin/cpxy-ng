//! Durable UUID identity helpers shared by Linux and mobile harnesses.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;
pub fn atomic_save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(
            path.parent()
                .ok_or_else(|| io::Error::other("missing state directory"))?,
        )?
        .sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
pub fn client_id(path: &Path) -> io::Result<Uuid> {
    let _lock = lock(path)?;
    match fs::read_to_string(path) {
        Ok(s) => Uuid::parse_str(s.trim()).map_err(|_| {
            io::Error::other(
                "invalid persisted client UUID; restore identity or explicitly provision a new one",
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let id = Uuid::new_v4();
            atomic_save(path, id.to_string().as_bytes())?;
            Ok(id)
        }
        Err(e) => Err(e),
    }
}
pub fn lock(path: &Path) -> io::Result<File> {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(PathBuf::from(name))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::other("address/identity state is already in use"));
    }
    Ok(file)
}
