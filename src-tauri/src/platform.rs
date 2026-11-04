//! OS glue for the password vault: user presence check + keychain-held key.

/// Ask the OS who's there: Touch ID, falling back to the login password. Blocks — never call on the main thread.
#[cfg(target_os = "macos")]
pub fn authenticate(reason: &str) -> bool {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    let (tx, rx) = std::sync::mpsc::channel();
    let reply = RcBlock::new(move |ok: Bool, _err: *mut NSError| { let _ = tx.send(ok.as_bool()); });
    unsafe {
        let ctx = LAContext::new();
        ctx.evaluatePolicy_localizedReason_reply(LAPolicy::DeviceOwnerAuthentication, &NSString::from_str(reason), &reply);
        rx.recv().unwrap_or(false)
    }
}

// ponytail: no Windows Hello / polkit prompt yet; the keychain (Credential Manager / Secret Service) is the only gate there.
#[cfg(not(target_os = "macos"))]
pub fn authenticate(_reason: &str) -> bool { true }

/// The 32-byte vault key from the OS keychain. Only created when `create` (no vault file yet),
/// so a missing key can never cause an existing vault to be overwritten with a new one.
pub fn vault_key(create: bool) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
    let entry = keyring::Entry::new("r-browser", "vault-key").map_err(|e| e.to_string())?;
    match entry.get_secret() {
        Ok(mut k) => {
            let key = <[u8; 32]>::try_from(k.as_slice()).map_err(|_| "vault key in the keychain is damaged".to_string());
            zeroize::Zeroize::zeroize(&mut k);
            key.map(zeroize::Zeroizing::new)
        }
        Err(keyring::Error::NoEntry) if create => {
            let k = crate::core::vault::new_key();
            entry.set_secret(&k).map_err(|e| e.to_string())?;
            Ok(zeroize::Zeroizing::new(k))
        }
        Err(keyring::Error::NoEntry) => Err("the vault key is missing from the keychain, so saved passwords can't be opened".into()),
        Err(e) => Err(e.to_string()),
    }
}
