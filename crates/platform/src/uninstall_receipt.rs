//! 当前用户 DPAPI 认证的卸载完成凭证；普通结果 JSON 不参与归属放行。
use crate::{PlatformError, Result, paths::PlatformPaths};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u8,
    sid: String,
    root: PathBuf,
    installation: serde_json::Value,
}
#[cfg(windows)]
fn protect(input: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let source = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &source,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &source,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(PlatformError("uninstall_receipt_invalid"));
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut _);
    };
    Ok(bytes)
}
#[cfg(windows)]
pub fn create(paths: &PlatformPaths, installation: serde_json::Value) -> Result<()> {
    let value = Receipt {
        schema: 1,
        sid: crate::windows::current_sid()?,
        root: paths
            .data
            .canonicalize()
            .map_err(|_| PlatformError("unsafe_data_dir"))?,
        installation,
    };
    let raw = serde_json::to_vec(&value).map_err(|_| PlatformError("uninstall_receipt_invalid"))?;
    let sealed = protect(&raw, true)?;
    let path = paths.data.join("uninstall-complete.receipt");
    agentnotify_core::queue::reject_links(&path)
        .map_err(|_| PlatformError("unsafe_uninstall_receipt"))?;
    use std::io::Write;
    let mut temp = tempfile::NamedTempFile::new_in(&paths.data)
        .map_err(|_| PlatformError("uninstall_receipt_write_failed"))?;
    temp.write_all(&sealed)
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|_| PlatformError("uninstall_receipt_write_failed"))?;
    temp.persist(path)
        .map_err(|_| PlatformError("uninstall_receipt_write_failed"))?;
    Ok(())
}
#[cfg(windows)]
pub(crate) fn installation(paths: &PlatformPaths) -> Result<Option<serde_json::Value>> {
    let p = paths.data.join("uninstall-complete.receipt");
    agentnotify_core::queue::reject_links(&p)
        .map_err(|_| PlatformError("unsafe_uninstall_receipt"))?;
    if !p.exists() {
        return Ok(None);
    }
    if fs::metadata(&p)
        .map_err(|_| PlatformError("uninstall_receipt_invalid"))?
        .len()
        > 65536
    {
        return Err(PlatformError("uninstall_receipt_invalid"));
    }
    let value: Receipt = serde_json::from_slice(&protect(
        &fs::read(p).map_err(|_| PlatformError("uninstall_receipt_invalid"))?,
        false,
    )?)
    .map_err(|_| PlatformError("uninstall_receipt_invalid"))?;
    if value.schema != 1
        || value.sid != crate::windows::current_sid()?
        || value.root
            != paths
                .data
                .canonicalize()
                .map_err(|_| PlatformError("unsafe_data_dir"))?
    {
        return Err(PlatformError("uninstall_receipt_invalid"));
    }
    Ok(Some(value.installation))
}
#[cfg(not(windows))]
pub fn create(_: &PlatformPaths, _: serde_json::Value) -> Result<()> {
    Err(PlatformError("uninstall_platform_unsupported"))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn completed_receipt_authenticates_user_root_and_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let a = PlatformPaths::override_data(&temp.path().join("a")).unwrap();
        fs::create_dir(&a.data).unwrap();
        assert!(installation(&a).unwrap().is_none());
        let marker = serde_json::json!({"id":"isolated-fixture"});
        create(&a, marker.clone()).unwrap();
        assert_eq!(installation(&a).unwrap(), Some(marker));
        let b = PlatformPaths::override_data(&temp.path().join("b")).unwrap();
        fs::create_dir(&b.data).unwrap();
        fs::copy(
            a.data.join("uninstall-complete.receipt"),
            b.data.join("uninstall-complete.receipt"),
        )
        .unwrap();
        assert!(installation(&b).is_err());
        let p = a.data.join("uninstall-complete.receipt");
        let mut bytes = fs::read(&p).unwrap();
        let index = bytes.len() / 2;
        bytes[index] ^= 1;
        fs::write(&p, bytes).unwrap();
        assert!(installation(&a).is_err());
        // 普通 partial 结果不产生认证完成凭证，不参与重装放行。
        fs::remove_file(p).unwrap();
        fs::write(
            a.data.join("uninstall-result.json"),
            b"{\"status\":\"incomplete\"}",
        )
        .unwrap();
        assert!(installation(&a).unwrap().is_none());
    }
}
