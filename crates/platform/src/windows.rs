//! Windows公开API：当前用户身份、ACL与生命周期路径固定，不防同用户恶意进程或管理员。
use crate::{PlatformError, Result};
use agentnotify_core::{
    contracts::{QueueError, QueueErrorCode},
    queue::{DirectorySecurity, error},
};
use std::{
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::Mutex,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::Authorization::*,
    Security::*,
    Storage::FileSystem::*,
    System::{Com::*, SystemServices::*, Threading::*, WindowsProgramming::DRIVE_FIXED},
    UI::Shell::*,
};
thread_local! { static LAST_VERIFY_ERROR: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) }; }
/// 当前线程最近一次目录验证的固定错误码，不含路径或系统错误文本。
pub fn take_last_verify_error() -> Option<&'static str> { LAST_VERIFY_ERROR.with(|value| value.take()) }
pub(crate) fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}
pub(crate) struct Handle(pub HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
pub fn local_data() -> Result<PathBuf> {
    unsafe {
        let mut p = null_mut();
        if SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, null_mut(), &mut p) < 0 {
            return Err(PlatformError("known_folder_unavailable"));
        }
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        let path = PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
            p, n,
        )));
        CoTaskMemFree(p.cast());
        Ok(path)
    }
}
pub fn current_sid() -> Result<String> {
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(PlatformError("identity_unavailable"));
        }
        let token = Handle(token);
        let mut n = 0;
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut n);
        let mut bytes = vec![0usize; (n as usize).div_ceil(size_of::<usize>())];
        if GetTokenInformation(token.0, TokenUser, bytes.as_mut_ptr().cast(), n, &mut n) == 0 {
            return Err(PlatformError("identity_unavailable"));
        }
        sid_text((*(bytes.as_ptr().cast::<TOKEN_USER>())).User.Sid)
    }
}
unsafe fn sid_text(sid: PSID) -> Result<String> {
    unsafe {
        if IsValidSid(sid) == 0 {
            return Err(PlatformError("invalid_sid"));
        }
        let mut p = null_mut();
        if ConvertSidToStringSidW(sid, &mut p) == 0 {
            return Err(PlatformError("identity_unavailable"));
        }
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        let text = String::from_utf16(std::slice::from_raw_parts(p, n))
            .map_err(|_| PlatformError("invalid_sid"));
        LocalFree(p.cast());
        text
    }
}
pub(crate) struct Descriptor(pub PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
pub(crate) fn descriptor(sid: &str) -> Result<Descriptor> {
    unsafe {
        let mut p = null_mut();
        let s = wide(format!(
            "O:{sid}D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
        ));
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(s.as_ptr(), 1, &mut p, null_mut())
            == 0
        {
            return Err(PlatformError("security_descriptor_failed"));
        }
        Ok(Descriptor(p))
    }
}
fn trusted(s: &str, user: &str) -> bool {
    s == user || s == "S-1-5-18" || s == "S-1-5-32-544"
}
/// 严格子树拒绝非受信主体的读取/写入；祖先拒绝可替换子路径的权限。
unsafe fn verify_acl(handle: HANDLE, user: &str, strict: bool) -> Result<()> {
    unsafe {
        let (mut owner, mut acl, mut sd) = (null_mut(), null_mut(), null_mut());
        if GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut sd,
        ) != 0
        {
            return Err(PlatformError("acl_unavailable"));
        }
        let _sd = Descriptor(sd);
        // Windows系统盘祖先可能由Windows Modules Installer持有；只放行这一固定owner。
        // 严格数据子树仍只允许当前用户/SYSTEM/Administrators，ACE检查不扩大。
        const TRUSTED_INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
        let owner_sid = sid_text(owner)?;
        let trusted_owner = trusted(&owner_sid, user) || (!strict && owner_sid == TRUSTED_INSTALLER);
        if acl.is_null() || !trusted_owner {
            return Err(PlatformError("unsafe_owner_or_acl"));
        }
        for i in 0..(*acl).AceCount as u32 {
            let mut ace = null_mut();
            if GetAce(acl, i, &mut ace) == 0 {
                return Err(PlatformError("acl_unavailable"));
            }
            let header = &*ace.cast::<ACE_HEADER>();
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE as u8
                && header.AceType != ACCESS_DENIED_ACE_TYPE as u8
            {
                return Err(PlatformError("unsupported_acl"));
            }
            if header.AceType == ACCESS_DENIED_ACE_TYPE as u8
                || header.AceFlags & INHERIT_ONLY_ACE as u8 != 0
            {
                continue;
            }
            let a = &*ace.cast::<ACCESS_ALLOWED_ACE>();
            let sid = std::ptr::addr_of!(a.SidStart) as PSID;
            if trusted(&sid_text(sid)?, user) {
                continue;
            }
            let dangerous = if strict {
                0xFFFF_FFFF
            } else {
                DELETE
                    | WRITE_DAC
                    | WRITE_OWNER
                    | FILE_DELETE_CHILD
                    | FILE_WRITE_ATTRIBUTES
                    | GENERIC_WRITE
                    | GENERIC_ALL
            };
            if a.Mask & dangerous != 0 {
                return Err(PlatformError("unsafe_acl"));
            }
        }
        Ok(())
    }
}
fn open_checked(path: &Path, user: &str, strict: bool) -> Result<Handle> {
    unsafe {
        let h = CreateFileW(
            wide(path).as_ptr(),
            READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            let code = match GetLastError() {
                ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => "directory_entry_missing",
                ERROR_SHARING_VIOLATION => "directory_entry_busy",
                ERROR_ACCESS_DENIED => "directory_access_denied",
                _ => "directory_handle_unavailable",
            };
            return Err(PlatformError(code));
        }
        let handle = Handle(h);
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(h, &mut info) == 0
            || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(PlatformError("unsafe_reparse_path"));
        }
        verify_acl(h, user, strict)?;
        let mut flags = 0;
        if GetVolumeInformationByHandleW(
            h,
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            null_mut(),
            0,
        ) == 0
            || flags & FILE_PERSISTENT_ACLS == 0
        {
            return Err(PlatformError("acl_filesystem_required"));
        }
        Ok(handle)
    }
}
/// 所有祖先、根和现有子目录的不可删除共享句柄持有到Queue释放。
pub struct SecureDirectory {
    root: PathBuf,
    user: String,
    guards: Mutex<Vec<(PathBuf, Handle)>>,
}
impl SecureDirectory {
    pub fn open(path: &Path, create: bool) -> Result<Self> {
        let text = path.as_os_str().to_string_lossy();
        if !path.is_absolute()
            || text.starts_with(r"\\")
            || text[2.min(text.len())..].contains(':')
            || text.contains('\0')
        {
            return Err(PlatformError("unsafe_data_dir"));
        }
        let drive = path
            .ancestors()
            .last()
            .ok_or(PlatformError("unsafe_data_dir"))?;
        if unsafe { GetDriveTypeW(wide(drive).as_ptr()) } != DRIVE_FIXED {
            return Err(PlatformError("local_fixed_disk_required"));
        }
        let user = current_sid()?;
        let mut guards = Vec::new();
        let mut ancestors: Vec<_> = path.ancestors().collect();
        ancestors.reverse();
        for ancestor in ancestors {
            if !ancestor.exists() {
                if !create || ancestor != path {
                    return Err(PlatformError("directory_missing"));
                }
                let sd = descriptor(&user)?;
                let sa = SECURITY_ATTRIBUTES {
                    nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: sd.0,
                    bInheritHandle: 0,
                };
                if unsafe { CreateDirectoryW(wide(ancestor).as_ptr(), &sa) } == 0
                    && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
                {
                    return Err(PlatformError("directory_create_failed"));
                }
            }
            guards.push((
                ancestor.to_owned(),
                open_checked(ancestor, &user, ancestor == path)?,
            ));
        }
        let result = Self {
            root: path.to_owned(),
            user,
            guards: Mutex::new(guards),
        };
        result.verify_tree()?;
        Ok(result)
    }
    /// 原子替换/SQLite临时文件会在枚举后消失；仅重新完整扫描，绝不略过未验证条目。
    fn verify_tree(&self) -> Result<()> {
        for attempt in 0..3 {
            match self.verify_tree_once() {
                Err(failure) if attempt < 2 && matches!(failure.0, "directory_entry_missing" | "directory_entry_busy") => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                },
                result => return result,
            }
        }
        Err(PlatformError("directory_scan_unavailable"))
    }
    /// 每轮重新枚举并逐项核验owner/DACL/reparse；权限错误不得进入重试豁免。
    fn verify_tree_once(&self) -> Result<()> {
        let mut guards = self
            .guards
            .lock()
            .map_err(|_| PlatformError("directory_lock_failed"))?;
        let mut todo = vec![self.root.clone()];
        let mut count = 0;
        while let Some(dir) = todo.pop() {
            for entry in
                std::fs::read_dir(&dir).map_err(|_| PlatformError("directory_unavailable"))?
            {
                let entry = entry.map_err(|_| PlatformError("directory_unavailable"))?;
                let path = entry.path();
                count += 1;
                if count > 100_000 {
                    return Err(PlatformError("directory_scan_limit"));
                }
                let h = open_checked(&path, &self.user, true)?;
                if entry
                    .file_type()
                    .map_err(|_| PlatformError("directory_unavailable"))?
                    .is_dir()
                {
                    todo.push(path.clone());
                    if !guards.iter().any(|(p, _)| p == &path) {
                        guards.push((path, h));
                    }
                }
            }
        }
        Ok(())
    }
}
impl DirectorySecurity for SecureDirectory {
    fn verify(&self, path: &Path) -> std::result::Result<(), QueueError> {
        LAST_VERIFY_ERROR.with(|value| value.set(None));
        if path.canonicalize().ok() != self.root.canonicalize().ok() || !path.exists() {
            LAST_VERIFY_ERROR.with(|value| value.set(Some("root_identity_changed")));
            return Err(error(QueueErrorCode::StorageUnavailable));
        }
        self.verify_tree().map_err(|failure| {
            LAST_VERIFY_ERROR.with(|value| value.set(Some(failure.0)));
            error(QueueErrorCode::StorageUnavailable)
        })
    }
}
