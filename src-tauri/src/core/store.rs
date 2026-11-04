use serde::{de::DeserializeOwned, Serialize};
use std::{fs, io, io::Write, path::Path};

/// Write to a temp file, fsync, then rename, so a crash never leaves a half-written file.
pub fn save_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut f = fs::File::create(&tmp)?;
    f.write_all(&serde_json::to_vec(value)?)?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

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
