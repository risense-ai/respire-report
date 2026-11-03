/// Exercise only a fresh random test target; never enumerate or read existing credentials.
#[cfg(windows)]
pub fn credential_roundtrip() -> Result<(), String> {
    // 1. Bind explicitly to the compiled native backend and assert its concrete type.
    let service = format!("AgentNotify.M0.{}", uuid::Uuid::new_v4());
    let entry = keyring::Entry::new(&service, "isolated-probe")
        .map_err(|_| "native entry creation failed")?;
    if !entry
        .get_credential()
        .is::<keyring::windows::WinCredential>()
    {
        return Err("blocked: credential backend is not Windows WinCredential".into());
    }
    struct Cleanup<'a>(&'a keyring::Entry);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.delete_credential();
        }
    }
    let _cleanup = Cleanup(&entry);
    let secret = uuid::Uuid::new_v4().as_bytes().to_vec();

    // 2. Round-trip the random bytes without printing either expected or actual secret.
    entry
        .set_secret(&secret)
        .map_err(|_| "native credential write denied/unavailable")?;
    let actual = entry
        .get_secret()
        .map_err(|_| "native credential read denied/unavailable")?;
    if actual != secret {
        return Err("native credential roundtrip mismatch".into());
    }

    // 3. Delete and prove absence. Drop also attempts cleanup on early returns.
    entry
        .delete_credential()
        .map_err(|_| "native credential cleanup failed")?;
    if !matches!(entry.get_secret(), Err(keyring::Error::NoEntry)) {
        return Err("native credential still exists or absence cannot be verified".into());
    }
    super::record(
        "credential_real_roundtrip",
        "Windows Credential Manager / WinCredential",
        "keyring=3.6.3",
        &["windows-native", "default-features=false"],
        "random binary test entry wrote/read/deleted; NoEntry verified; secret never logged",
    )
}

#[cfg(not(windows))]
pub fn credential_roundtrip() -> Result<(), String> {
    Err("blocked: this M0 native credential probe requires Windows; no mock fallback".into())
}
