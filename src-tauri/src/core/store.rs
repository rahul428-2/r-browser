use serde::{de::DeserializeOwned, Serialize};
use std::{fs, io, io::Write, path::Path};

/// Write to a temp file (owner-only on unix), fsync, then rename, so a crash never leaves a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

pub fn save_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> { write_atomic(path, &serde_json::to_vec(value)?) }

/// None if missing or unreadable; a corrupt file is kept as `<name>.json.bad` for inspection.
pub fn load<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(_) => {
            let _ = fs::rename(path, path.with_extension("json.bad"));
            None
        }
    }
}
