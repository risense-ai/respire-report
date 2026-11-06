//! 当前用户Windows Credential Manager；其他目标未启用后端时明确Unavailable，无mock/文件后备。
use agentnotify_core::{contracts::*, queue::AeadBodyProtector};
use ring::rand::{SecureRandom, SystemRandom};
pub struct NativeCredentialStore {
    namespace: String,
}
impl Default for NativeCredentialStore {
    fn default() -> Self {
        Self {
            namespace: "AgentNotify.Credentials.v1".into(),
        }
    }
}
impl NativeCredentialStore {
    /// 入站签名和来源校验使用与邮箱、正文密钥隔离的原生凭据命名空间。
    pub(crate) fn for_ingress(namespace: String) -> Self {
        Self { namespace }
    }

    /// 只允许新的UUID隔离命名空间，供显式授权的原生后端测试使用，不能枚举真实条目。
    pub fn isolated(id: uuid::Uuid) -> Self {
        Self {
            namespace: format!("AgentNotify.Test.{id}"),
        }
    }
    #[cfg(windows)]
    fn entry(&self, r: &CredentialRef) -> Result<keyring::Entry, CredentialError> {
        let entry = keyring::Entry::new(&self.namespace, &r.id.to_string()).map_err(map_error)?;
        if !entry
            .get_credential()
            .is::<keyring::windows::WinCredential>()
        {
            return Err(fail(CredentialErrorCode::Unavailable));
        }
        Ok(entry)
    }
    /// 只读已有引用；任何读取失败都不覆盖密钥，已有密文继续保持可恢复。
    pub fn load_content_key(
        &self,
        r: &CredentialRef,
    ) -> Result<AeadBodyProtector, CredentialError> {
        AeadBodyProtector::from_key(self.get(r)?)
            .map_err(|_| fail(CredentialErrorCode::ReadbackFailed))
    }
    /// 每次显式初始化创建全新随机引用，不共享固定条目，不会覆盖并发初始化者。
    /// 调用者必须在宿主配置锁内发布返回的引用；失去发布竞争者应显式删除自己的未使用引用。
    pub fn create_content_key(
        &self,
    ) -> Result<(CredentialRef, AeadBodyProtector), CredentialError> {
        let r = CredentialRef {
            id: uuid::Uuid::new_v4(),
        };
        let mut bytes = zeroize::Zeroizing::new(vec![0; 32]);
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| fail(CredentialErrorCode::Unavailable))?;
        self.put(&r, SecretInput::new(bytes.to_vec()))?;
        let protector = self.load_content_key(&r)?;
        Ok((r, protector))
    }
}
fn fail(code: CredentialErrorCode) -> CredentialError {
    CredentialError { code }
}
#[cfg(windows)]
fn map_error(e: keyring::Error) -> CredentialError {
    use keyring::Error;
    fail(match e {
        Error::NoEntry => CredentialErrorCode::Missing,
        Error::NoStorageAccess(_) => CredentialErrorCode::AccessDenied,
        Error::PlatformFailure(_) => CredentialErrorCode::Unavailable,
        _ => CredentialErrorCode::Unavailable,
    })
}

#[cfg(windows)]
struct Manifest {
    length: usize,
    digest: [u8; 32],
    parts: Vec<uuid::Uuid>,
}
#[cfg(windows)]
impl Manifest {
    /// 清单只含长度、SHA256与本命名空间随机分片ID；不含明文秘密。
    fn decode(bytes: &[u8]) -> Result<Self, CredentialError> {
        if bytes.len() < 46 || &bytes[..8] != b"ANCRV001" {
            return Err(fail(CredentialErrorCode::ReadbackFailed));
        }
        let length = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let count = u16::from_le_bytes(bytes[44..46].try_into().unwrap()) as usize;
        if length > 65536
            || count > 32
            || count != length.div_ceil(2048)
            || bytes.len() != 46 + count * 16
        {
            return Err(fail(CredentialErrorCode::ReadbackFailed));
        }
        Ok(Self {
            length,
            digest: bytes[12..44].try_into().unwrap(),
            parts: bytes[46..]
                .chunks_exact(16)
                .map(|v| uuid::Uuid::from_bytes(v.try_into().unwrap()))
                .collect(),
        })
    }
    /// 摘要覆盖完整清单和每次发布新建的随机分片ID，而不只比较token明文摘要。
    fn revision(&self) -> CredentialRevision {
        CredentialRevision::from_bytes(
            ring::digest::digest(&ring::digest::SHA256, &self.encode())
                .as_ref()
                .try_into()
                .unwrap(),
        )
    }
    fn encode(&self) -> Vec<u8> {
        let mut bytes = b"ANCRV001".to_vec();
        bytes.extend((self.length as u32).to_le_bytes());
        bytes.extend(self.digest);
        bytes.extend((self.parts.len() as u16).to_le_bytes());
        for p in &self.parts {
            bytes.extend(p.as_bytes());
        }
        bytes
    }
}
#[cfg(windows)]
impl NativeCredentialStore {
    fn part(&self, id: uuid::Uuid) -> Result<keyring::Entry, CredentialError> {
        let entry = keyring::Entry::new(&format!("{}.part", self.namespace), &id.to_string())
            .map_err(map_error)?;
        if !entry
            .get_credential()
            .is::<keyring::windows::WinCredential>()
        {
            return Err(fail(CredentialErrorCode::Unavailable));
        }
        Ok(entry)
    }

    fn remove_parts(&self, manifest: &Manifest) -> Result<(), CredentialError> {
        for id in &manifest.parts {
            match self.part(*id)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(map_error(e)),
            }
        }
        Ok(())
    }
    fn pending(&self, r: &CredentialRef) -> Result<keyring::Entry, CredentialError> {
        keyring::Entry::new(&format!("{}.pending", self.namespace), &r.id.to_string())
            .map_err(map_error)
    }
    /// 同一引用的跨进程mutex覆盖读取/替换/删除和恢复；2秒未获得即失败关闭。
    fn lock(&self, r: &CredentialRef) -> Result<NativeLock, CredentialError> {
        NativeLock::acquire(&self.namespace, r)
    }
    /// 原生intent在创建任何分片之前落下；只恢复此引用，不枚举其他账户。
    fn recover(&self, r: &CredentialRef) -> Result<(), CredentialError> {
        let pending = self.pending(r)?;
        let raw = match pending.get_secret() {
            Ok(v) => v,
            Err(keyring::Error::NoEntry) => return Ok(()),
            Err(e) => return Err(map_error(e)),
        };
        if raw.len() < 12 || &raw[..8] != b"ANCTX001" {
            return Err(fail(CredentialErrorCode::ReadbackFailed));
        }
        let n = u16::from_le_bytes(raw[8..10].try_into().unwrap()) as usize;
        let old_len = u16::from_le_bytes(raw[10..12].try_into().unwrap()) as usize;
        if raw.len() != 12 + n + old_len {
            return Err(fail(CredentialErrorCode::ReadbackFailed));
        }
        let next = Manifest::decode(&raw[12..12 + n])?;
        let previous = if old_len == 0 {
            None
        } else {
            Some(Manifest::decode(&raw[12 + n..])?)
        };
        let root = self.entry(r)?;
        let live = match root.get_secret() {
            Ok(v) => Some(v),
            Err(keyring::Error::NoEntry) => None,
            Err(e) => return Err(map_error(e)),
        };
        if live.as_deref() == Some(&raw[12..12 + n]) {
            // 已发布的新版本必须仍完整才能清理旧分片；权限/完整性失败保留intent与旧条目。
            self.read_manifest(&next)?;
            if let Some(old) = &previous {
                self.remove_parts(old)?;
            }
        } else if live.as_deref() == previous.as_ref().map(|_| &raw[12 + n..]) {
            self.remove_parts(&next)?;
        } else if live.is_none() {
            // 显式删除中断后继续清理；没有root不能重新发布任何历史秘密。
            self.remove_parts(&next)?;
            if let Some(old) = &previous {
                self.remove_parts(old)?;
            }
        } else {
            return Err(fail(CredentialErrorCode::ReadbackFailed));
        }
        pending.delete_credential().map_err(map_error)
    }
    fn read_manifest(&self, manifest: &Manifest) -> Result<CredentialHandle, CredentialError> {
        let mut secret = zeroize::Zeroizing::new(Vec::with_capacity(manifest.length));
        for id in &manifest.parts {
            let bytes = zeroize::Zeroizing::new(self.part(*id)?.get_secret().map_err(map_error)?);
            if bytes.len() > 2048 {
                return Err(fail(CredentialErrorCode::ReadbackFailed));
            }
            secret.extend_from_slice(&bytes);
        }
        if secret.len() != manifest.length
            || ring::digest::digest(&ring::digest::SHA256, &secret).as_ref() != manifest.digest
        {
            return Err(fail(CredentialErrorCode::ReadbackFailed));
        }
        let secret = SecretInput::new(std::mem::take(&mut *secret));
        // OAuth集合总是非空；每次发布使用新的随机part IDs，即使token内容相同也不是旧版本。
        // 历史空秘密没有随机分片，不能提供具备ABA防护的版本，因此不支持条件刷新。
        if manifest.length == 0 {
            return Ok(CredentialHandle::from_secret(secret));
        }
        Ok(CredentialHandle::from_versioned_secret(
            secret,
            manifest.revision(),
        ))
    }
}

#[cfg(windows)]
impl NativeCredentialStore {
    /// 仅由持有同引用NativeLock并完成recover的put/CAS调用。
    fn put_locked(&self, r: &CredentialRef, input: SecretInput) -> Result<(), CredentialError> {
        // 1. 调用者已持有本引用锁并完成恢复；旧凭据不可读时禁止覆盖/重置。
        let root = self.entry(r)?;
        let previous = match root.get_secret() {
            Ok(b) => Some(Manifest::decode(&b)?),
            Err(keyring::Error::NoEntry) => None,
            Err(e) => return Err(map_error(e)),
        };
        let length = input.with_secret(|b| b.len());
        if length > 65536 {
            return Err(fail(CredentialErrorCode::WriteFailed));
        }
        let manifest = Manifest {
            length,
            digest: input.with_secret(|b| {
                ring::digest::digest(&ring::digest::SHA256, b)
                    .as_ref()
                    .try_into()
                    .unwrap()
            }),
            parts: (0..length.div_ceil(2048))
                .map(|_| uuid::Uuid::new_v4())
                .collect(),
        };
        let new_bytes = manifest.encode();
        let old_bytes = previous.as_ref().map(Manifest::encode).unwrap_or_default();
        let mut intent = b"ANCTX001".to_vec();
        intent.extend((new_bytes.len() as u16).to_le_bytes());
        intent.extend((old_bytes.len() as u16).to_le_bytes());
        intent.extend(&new_bytes);
        intent.extend(&old_bytes);
        self.pending(r)?.set_secret(&intent).map_err(map_error)?;
        // 2. 全部分片写好并按摘要读回后才切root，失败保留旧root；intent让下次调用可有界清理。
        input.with_secret(|bytes| -> Result<(), CredentialError> {
            for (part, data) in manifest.parts.iter().zip(bytes.chunks(2048)) {
                self.part(*part)?.set_secret(data).map_err(map_error)?;
            }
            Ok(())
        })?;
        let actual = self.read_manifest(&manifest)?;
        if !input.with_secret(|b| actual.with_secret(|a| a == b)) {
            return Err(fail(CredentialErrorCode::ReadbackFailed));
        }
        root.set_secret(&new_bytes).map_err(map_error)?;
        // 3. root成功发布后恢复逻辑清旧分片与intent；失败不回退新版本，保留恢复证据并报告错误。
        self.recover(r)
    }
}

#[cfg(windows)]
struct NativeLock(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl NativeLock {
    fn acquire(namespace: &str, r: &CredentialRef) -> Result<Self, CredentialError> {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0},
            System::Threading::{CreateMutexW, WaitForSingleObject},
        };
        let hash = ring::digest::digest(
            &ring::digest::SHA256,
            format!("{namespace}/{}", r.id).as_bytes(),
        );
        let suffix = hash
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let name: Vec<u16> = format!("Global\\AgentNotify.Credential.{suffix}\0")
            .encode_utf16()
            .collect();
        // 默认安全描述符来自当前进程token；没有用户权限就拒绝，绝不打开旁路锁。
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(fail(CredentialErrorCode::AccessDenied));
        }
        let result = unsafe { WaitForSingleObject(handle, 2000) };
        if result != WAIT_OBJECT_0 && result != WAIT_ABANDONED {
            unsafe { CloseHandle(handle) };
            return Err(fail(CredentialErrorCode::Locked));
        }
        Ok(Self(handle))
    }
}
#[cfg(windows)]
impl Drop for NativeLock {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.0);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
#[cfg(windows)]
impl CredentialStore for NativeCredentialStore {
    fn get(&self, r: &CredentialRef) -> Result<CredentialHandle, CredentialError> {
        let _lock = self.lock(r)?;
        self.recover(r)?;
        let manifest = Manifest::decode(&self.entry(r)?.get_secret().map_err(map_error)?)?;
        self.read_manifest(&manifest)
    }
    fn put(&self, r: &CredentialRef, input: SecretInput) -> Result<(), CredentialError> {
        let _lock = self.lock(r)?;
        self.recover(r)?;
        self.put_locked(r, input)
    }
    fn replace_if_revision(
        &self,
        r: &CredentialRef,
        expected: &CredentialRevision,
        input: SecretInput,
    ) -> Result<bool, CredentialError> {
        // 与delete/普通put共用同一原生mutex；网络刷新不在此锁内。
        let _lock = self.lock(r)?;
        self.recover(r)?;
        let manifest = match self.entry(r)?.get_secret() {
            Ok(raw) => Manifest::decode(&raw)?,
            Err(keyring::Error::NoEntry) => return Ok(false),
            Err(e) => return Err(map_error(e)),
        };
        if manifest.length == 0 || &manifest.revision() != expected {
            return Ok(false);
        }
        // 检查之后继续持锁直至新清单发布；不会从Missing路径创建已撤销的引用。
        self.put_locked(r, input)?;
        Ok(true)
    }
    fn delete(&self, r: &CredentialRef) -> Result<(), CredentialError> {
        let _lock = self.lock(r)?;
        self.recover(r)?;
        let root = self.entry(r)?;
        let raw = match root.get_secret() {
            Ok(b) => b,
            Err(keyring::Error::NoEntry) => return Ok(()),
            Err(e) => return Err(map_error(e)),
        };
        let manifest = Manifest::decode(&raw)?;
        // 用同一intent协议登记删除范围；root先移除，失败清理可在后续delete/get恢复，绝不复活。
        let mut intent = b"ANCTX001".to_vec();
        intent.extend((raw.len() as u16).to_le_bytes());
        intent.extend(0u16.to_le_bytes());
        intent.extend(&raw);
        self.pending(r)?.set_secret(&intent).map_err(map_error)?;
        root.delete_credential().map_err(map_error)?;
        self.remove_parts(&manifest)?;
        self.pending(r)?.delete_credential().map_err(map_error)
    }
}
#[cfg(not(windows))]
impl CredentialStore for NativeCredentialStore {
    fn get(&self, _: &CredentialRef) -> Result<CredentialHandle, CredentialError> {
        Err(fail(CredentialErrorCode::Unavailable))
    }
    fn put(&self, _: &CredentialRef, _: SecretInput) -> Result<(), CredentialError> {
        Err(fail(CredentialErrorCode::Unavailable))
    }
    fn delete(&self, _: &CredentialRef) -> Result<(), CredentialError> {
        Err(fail(CredentialErrorCode::Unavailable))
    }
}
