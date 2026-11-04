//! Encrypted password vault. Pure logic: the key comes from the OS keychain (see platform.rs).
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use serde::{Deserialize, Serialize};
use std::path::Path;
use zeroize::{Zeroize, ZeroizeOnDrop};

const MAGIC: &[u8; 4] = b"RBV1";

/// `origin` = `scheme://host[:port]`; only an exact match is ever filled.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Zeroize, ZeroizeOnDrop)]
pub struct Entry {
    pub origin: String,
    pub username: String,
    pub password: String,
    pub note: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Vault {
    pub entries: Vec<Entry>,
}

/// http(s) origin of a URL; IDN hosts come out as punycode, so lookalikes never equal the real site.
pub fn origin_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.origin().ascii_serialization())
}

impl Vault {
    /// Same origin + username replaces the old password.
    pub fn add(&mut self, e: Entry) {
        match self.entries.iter_mut().find(|x| x.origin == e.origin && x.username == e.username) {
            Some(x) => *x = e,
            None => self.entries.push(e),
        }
    }

    pub fn find(&self, origin: &str) -> Vec<&Entry> { self.entries.iter().filter(|e| e.origin == origin).collect() }

    pub fn remove(&mut self, origin: &str, username: &str) { self.entries.retain(|e| !(e.origin == origin && e.username == username)); }

    /// `RBV1 | 12-byte random nonce | AES-256-GCM(json)`; the header is authenticated too.
    pub fn seal(&self, key: &[u8; 32]) -> Vec<u8> {
        let mut plain = serde_json::to_vec(self).expect("vault serializes");
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ct = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key))
            .encrypt(&nonce, Payload { msg: &plain, aad: MAGIC })
            .expect("aes-gcm encrypt");
        plain.zeroize();
        [&MAGIC[..], &nonce, &ct].concat()
    }

    pub fn open(blob: &[u8], key: &[u8; 32]) -> Result<Vault, String> {
        if blob.len() < 4 + 12 + 16 || &blob[..4] != MAGIC { return Err("not a vault file".into()); }
        let mut plain = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key))
            .decrypt(Nonce::from_slice(&blob[4..16]), Payload { msg: &blob[16..], aad: MAGIC })
            .map_err(|_| "vault could not be decrypted (wrong key or damaged file)")?;
        let v = serde_json::from_slice(&plain).map_err(|e| e.to_string());
        plain.zeroize();
        v
    }
}

/// A fresh random 256-bit key.
pub fn new_key() -> [u8; 32] { Aes256Gcm::generate_key(OsRng).into() }

/// Google Password Manager export: `name,url,username,password,note`. Non-web rows (android://) are skipped.
pub fn import_google_csv(text: &str) -> Result<Vec<Entry>, String> {
    let mut r = csv::ReaderBuilder::new().flexible(true).from_reader(text.as_bytes());
    let head = r.headers().map_err(|e| e.to_string())?.clone();
    let col = |n: &str| head.iter().position(|h| h.trim().eq_ignore_ascii_case(n));
    let (Some(url), Some(user), Some(pass)) = (col("url"), col("username"), col("password")) else {
        return Err("this isn't a Google Passwords export (needs url, username, password columns)".into());
    };
    let note = col("note");
    let mut out = Vec::new();
    for row in r.records() {
        let row = row.map_err(|e| e.to_string())?;
        let get = |i: usize| row.get(i).unwrap_or("").to_string();
        let (Some(origin), p) = (origin_of(&get(url)), get(pass)) else { continue };
        if p.is_empty() { continue; }
        out.push(Entry { origin, username: get(user), password: p, note: note.map(get).unwrap_or_default() });
    }
    Ok(out)
}

/// Overwrite with zeros, flush, delete. SSDs and snapshots can still keep old blocks — the UI says so.
pub fn shred(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let len = std::fs::metadata(path)?.len() as usize;
    let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.write_all(&vec![0u8; len])?;
    f.sync_all()?;
    drop(f);
    std::fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    const K: [u8; 32] = [7; 32];
    fn e(o: &str, u: &str, p: &str) -> Entry { Entry { origin: o.into(), username: u.into(), password: p.into(), note: String::new() } }

    #[test] fn seal_open_roundtrip() {
        let mut v = Vault::default();
        v.add(e("https://a.com", "me", "pw"));
        let blob = v.seal(&K);
        assert!(!blob.windows(2).any(|w| w == b"pw"), "no plaintext in the blob");
        assert_eq!(Vault::open(&blob, &K).unwrap(), v);
    }
    #[test] fn wrong_key_fails() {
        let blob = Vault::default().seal(&K);
        assert!(Vault::open(&blob, &[8; 32]).is_err());
    }
    #[test] fn tampered_blob_fails() {
        let mut blob = Vault::default().seal(&K);
        let n = blob.len() - 1;
        blob[n] ^= 1;
        assert!(Vault::open(&blob, &K).is_err());
        assert!(Vault::open(b"junk", &K).is_err());
    }
    #[test] fn find_exact_origin_only() {
        let mut v = Vault::default();
        v.add(e("https://a.com", "me", "pw"));
        assert_eq!(v.find("https://a.com").len(), 1);
        assert!(v.find("https://sub.a.com").is_empty());
        assert!(v.find("http://a.com").is_empty());
        assert_eq!(origin_of("https://a.com/login?x=1").as_deref(), Some("https://a.com"));
        assert_eq!(origin_of("https://a.com.evil.io/").as_deref(), Some("https://a.com.evil.io"));
        assert_eq!(origin_of("https://gооgle.com/").as_deref(), Some("https://xn--ggle-55da.com"), "IDN shown as punycode");
        assert_eq!(origin_of("rb://passwords"), None);
    }
    #[test] fn add_updates_same_user() {
        let mut v = Vault::default();
        v.add(e("https://a.com", "me", "old"));
        v.add(e("https://a.com", "me", "new"));
        v.add(e("https://a.com", "you", "x"));
        assert_eq!(v.entries.len(), 2);
        assert_eq!(v.find("https://a.com")[0].password, "new");
        v.remove("https://a.com", "me");
        assert_eq!(v.entries.len(), 1);
    }
    #[test] fn import_google_csv_parses_sample() {
        let csv = "name,url,username,password,note\n\
                   a.com,https://a.com/login,me@x.com,p1,\n\
                   b,https://b.org/,bob,\"p,2\",hi\n\
                   bad,android://xyz@com.app/,u,p3,\n";
        let got = import_google_csv(csv).unwrap();
        assert_eq!(got.len(), 2, "non-web rows skipped");
        assert_eq!((got[0].origin.as_str(), got[0].username.as_str(), got[0].password.as_str()), ("https://a.com", "me@x.com", "p1"));
        assert_eq!((got[1].password.as_str(), got[1].note.as_str()), ("p,2", "hi"));
        assert!(import_google_csv("foo,bar\n1,2\n").is_err(), "not a Google export");
    }
    #[test] fn shred_removes_file() {
        let p = std::env::temp_dir().join(format!("rb-shred-{}", std::process::id()));
        std::fs::write(&p, "secret").unwrap();
        shred(&p).unwrap();
        assert!(!p.exists());
    }
}
