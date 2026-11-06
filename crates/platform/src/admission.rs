//! 本机通用来源的显式授权，以及 IPC / 离线暂存的完整性验证。
use crate::{PlatformError, Result, credentials::NativeCredentialStore, paths::PlatformPaths};
use agentnotify_core::{
    contracts::{
        CredentialError, CredentialErrorCode, CredentialRef, CredentialStore, QueueError,
        QueueErrorCode, SecretInput,
    },
    event::{AdapterOutput, EventKind, OriginEvidence},
    queue::{SpoolAdmission, error, reject_links},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::PathBuf,
    sync::OnceLock,
    time::{Duration, Instant},
};
use uuid::Uuid;
use zeroize::Zeroizing;

const SOURCE_DOMAIN: &[u8] = b"AgentNotify Generic Source v1";
const IPC_DOMAIN: &[u8] = b"AgentNotify Local Ingress v1";
const SPOOL_DOMAIN: &[u8] = b"AgentNotify Spool v1";

/// 通用来源授权信封独立于中立事件契约；凭据只在 stdin 接收后瞬态持有。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenericSubmission {
    // 来源凭据：由接入者保存并经 stdin 提交，仅用于核验当前实例授权。
    credential: String,
    // 中立事件：通过核验后重新序列化并交给已有 v1 事件解析器。
    event: serde_json::Value,
}

pub struct IngressAdmission {
    // 宿主根目录：用于隔离每个数据目录的凭据和首次初始化锁。
    root: PathBuf,
    // 已读取的本机签名密钥：仅在当前进程内使用，不进入序列化。
    key: OnceLock<hmac::Key>,
}

impl IngressAdmission {
    pub fn open(paths: &PlatformPaths) -> Result<Self> {
        let root = paths
            .data
            .canonicalize()
            .map_err(|_| PlatformError("ingress_key_unavailable"))?;
        Ok(Self {
            root,
            key: OnceLock::new(),
        })
    }

    /// 显式授权创建一个全新实例，旧实例撤销后不可重新启用。
    pub fn enable_generic(&self, name: &str, input: &[u8]) -> Result<Uuid> {
        let token = decode_token(input.trim_ascii())?;
        let store = self.source_store()?;
        confirm_source(name)?;
        let id = Uuid::new_v4();
        let verifier = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, &token[..]),
            SOURCE_DOMAIN,
        );
        store
            .put(
                &CredentialRef { id },
                SecretInput::new(verifier.as_ref().to_vec()),
            )
            .map_err(source_store_error)?;
        Ok(id)
    }

    /// 从安全存储删除授权，后续入口和暂存重放都会拒绝该实例。
    pub fn revoke_generic(&self, id: Uuid) -> Result<()> {
        self.source_store()?
            .delete(&CredentialRef { id })
            .map_err(source_store_error)
    }

    /// 校验通用凭据并返回通过授权的实例 ID 和标准事件。原生 Hook 从不走此路径。
    pub fn authorize_generic(&self, raw: &[u8], expected: Option<Uuid>) -> Result<(Uuid, Vec<u8>)> {
        // 1. 解析外层授权信封，拆开后凭据和事件只在本进程内使用。
        let GenericSubmission { credential, event } =
            serde_json::from_slice(raw).map_err(|_| PlatformError("source_credential_required"))?;
        let credential = Zeroizing::new(credential);
        let token = decode_token(credential.as_bytes())?;
        let id = event
            .get("instance_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(PlatformError("invalid_identity"))?;
        // Hook 命令登记的实例与事件声明不一致时立即拒绝，不用正文替换实例。
        if expected.is_some_and(|expected| expected != id) {
            return Err(PlatformError("source_not_authorized"));
        }
        // 2. 通过原生存储中的校验值认证该实例，绝不将秘密转交事件解析器。
        let verifier = self
            .source_store()?
            .get(&CredentialRef { id })
            .map_err(source_store_error)?;
        let key = hmac::Key::new(hmac::HMAC_SHA256, &token[..]);
        verifier
            .with_secret(|stored| hmac::verify(&key, SOURCE_DOMAIN, stored))
            .map_err(|_| PlatformError("source_not_authorized"))?;
        let raw = serde_json::to_vec(&event).map_err(|_| PlatformError("invalid_input"))?;
        Ok((id, raw))
    }

    /// 允许已登记的通用完成、阻塞、失败和中断类报告，禁止伪装具名来源。
    pub fn check_source(&self, output: &AdapterOutput) -> Result<()> {
        let event = output.notification.as_ref();
        let caller = output.source_evidence.origin_evidence == OriginEvidence::CallerReported;
        // 具名适配器由 CLI 校验绑定并签发，后台仅信任其已认证的本地信封。
        if !caller && event.is_none_or(|e| e.adapter_id != "generic") {
            return Ok(());
        }
        // 上游事件 ID 必须稳定，保证同一来源重试可由现有事务去重。
        let event = event
            .filter(|e| caller && e.adapter_id == "generic" && e.event_id.is_some())
            .filter(|e| {
                matches!(
                    e.kind,
                    EventKind::ResponseFinished
                        | EventKind::TaskCompleted
                        | EventKind::AttentionRequired
                        | EventKind::ExecutionFailed
                        | EventKind::Interrupted
                )
            })
            .ok_or(PlatformError("source_not_authorized"))?;
        // 通用实例不能提供生命周期事实，避免其间接驱动看门狗。
        if output.lifecycle.is_some() {
            return Err(PlatformError("source_not_authorized"));
        }
        self.check_generic_active(event.instance_id)
    }

    pub fn check_generic_active(&self, id: Uuid) -> Result<()> {
        self.source_store()?
            .get(&CredentialRef { id })
            .map(|_| ())
            .map_err(source_store_error)
    }

    /// 签发后台请求的身份凭证，覆盖入站 ID、标准化事件和离席决定。
    pub fn sign_request(
        &self,
        id: Uuid,
        output: &AdapterOutput,
        suppressed: bool,
    ) -> Result<[u8; 32]> {
        let payload = request_payload(id, output, suppressed)?;
        Ok(authenticate(self.key(true)?, IPC_DOMAIN, &payload))
    }

    /// 先验证请求完整性，再检查可撤销的来源授权。
    pub fn verify_request(
        &self,
        id: Uuid,
        output: &AdapterOutput,
        suppressed: bool,
        tag: &[u8],
    ) -> Result<()> {
        let payload = request_payload(id, output, suppressed)?;
        verify(self.key(false)?, IPC_DOMAIN, &payload, tag)?;
        self.check_source(output)
    }

    fn store_for(&self, purpose: &str) -> NativeCredentialStore {
        let hash = Sha256::digest(self.root.as_os_str().as_encoded_bytes());
        NativeCredentialStore::for_ingress(format!(
            "AgentNotify.{purpose}.v1.{}",
            URL_SAFE_NO_PAD.encode(hash)
        ))
    }

    /// 通用来源绑定到本机安装 ID。同路径重装后旧来源凭据不再有效。
    fn source_store(&self) -> Result<NativeCredentialStore> {
        #[derive(Deserialize)]
        struct InstallationIdentity {
            // 安装记录版本：仅接受已有 v1 布局。
            schema_version: u8,
            // 安装 ID：原地升级保留，重装重新生成，用于隔离来源授权。
            id: Uuid,
        }
        let path = self.root.join("installation.json");
        reject_links(&path).map_err(|_| PlatformError("source_installation_unavailable"))?;
        let file =
            fs::File::open(path).map_err(|_| PlatformError("source_installation_unavailable"))?;
        let mut raw = Vec::new();
        file.take(16385)
            .read_to_end(&mut raw)
            .map_err(|_| PlatformError("source_installation_unavailable"))?;
        if raw.len() > 16384 {
            return Err(PlatformError("source_installation_unavailable"));
        }
        let installation: InstallationIdentity = serde_json::from_slice(&raw)
            .map_err(|_| PlatformError("source_installation_unavailable"))?;
        if installation.schema_version != 1 || installation.id.is_nil() {
            return Err(PlatformError("source_installation_unavailable"));
        }
        Ok(self.store_for(&format!("Sources.{}", installation.id)))
    }

    /// 首次签名时由跨进程锁串行创建密钥。遇到读错误保留既有密钥和离线记录。
    fn key(&self, create: bool) -> Result<&hmac::Key> {
        if let Some(key) = self.key.get() {
            return Ok(key);
        }
        let store = self.store_for("Ingress");
        let reference = CredentialRef { id: Uuid::nil() };
        let handle = match store.get(&reference) {
            Ok(handle) => handle,
            Err(e) if create && e.code == CredentialErrorCode::Missing => {
                let path = self.root.join("ingress-key.lock");
                reject_links(&path).map_err(|_| PlatformError("ingress_key_unavailable"))?;
                let lock = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(path)
                    .map_err(|_| PlatformError("ingress_key_unavailable"))?;
                let deadline = Instant::now() + Duration::from_millis(300);
                loop {
                    match lock.try_lock() {
                        Ok(()) => break,
                        Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => return Err(PlatformError("ingress_key_busy")),
                    }
                }
                match store.get(&reference) {
                    Ok(handle) => handle,
                    Err(e) if e.code == CredentialErrorCode::Missing => {
                        let marker = self.root.join("ingress-key.initialized");
                        reject_links(&marker)
                            .map_err(|_| PlatformError("ingress_key_unavailable"))?;
                        // 已经初始化却丢失密钥时保留旧暂存，避免新密钥把旧签名误判为篡改。
                        if marker
                            .try_exists()
                            .map_err(|_| PlatformError("ingress_key_unavailable"))?
                        {
                            return Err(PlatformError("ingress_key_unavailable"));
                        }
                        let mut key = Zeroizing::new([0u8; 32]);
                        SystemRandom::new()
                            .fill(&mut key[..])
                            .map_err(|_| PlatformError("ingress_key_unavailable"))?;
                        store
                            .put(&reference, SecretInput::new(key.to_vec()))
                            .map_err(|_| PlatformError("ingress_key_unavailable"))?;
                        let handle = store
                            .get(&reference)
                            .map_err(|_| PlatformError("ingress_key_unavailable"))?;
                        OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&marker)
                            .and_then(|file| file.sync_all())
                            .map_err(|_| PlatformError("ingress_key_unavailable"))?;
                        handle
                    }
                    Err(_) => return Err(PlatformError("ingress_key_unavailable")),
                }
            }
            Err(_) => return Err(PlatformError("ingress_key_unavailable")),
        };
        let key = handle
            .with_secret(|secret| {
                (secret.len() == 32).then(|| hmac::Key::new(hmac::HMAC_SHA256, secret))
            })
            .ok_or(PlatformError("ingress_key_unavailable"))?;
        Ok(self.key.get_or_init(|| key))
    }
}

impl SpoolAdmission for IngressAdmission {
    fn sign(&self, payload: &[u8]) -> std::result::Result<[u8; 32], QueueError> {
        let key = self
            .key(true)
            .map_err(|_| error(QueueErrorCode::BodyProtectionUnavailable))?;
        Ok(authenticate(key, SPOOL_DOMAIN, payload))
    }

    fn verify(&self, payload: &[u8], tag: &[u8]) -> std::result::Result<(), QueueError> {
        let key = self
            .key(false)
            .map_err(|_| error(QueueErrorCode::BodyProtectionUnavailable))?;
        verify(key, SPOOL_DOMAIN, payload, tag).map_err(|_| error(QueueErrorCode::InvalidInput))
    }

    fn check_source(&self, output: &AdapterOutput) -> std::result::Result<(), QueueError> {
        self.check_source(output).map_err(|e| {
            error(if e.0 == "source_not_authorized" {
                QueueErrorCode::InvalidInput
            } else {
                QueueErrorCode::BodyProtectionUnavailable
            })
        })
    }
}

/// 用相同的字段顺序构造签发和验签输入，包含全部标准化事件及本机抑制决定。
fn request_payload(id: Uuid, output: &AdapterOutput, suppressed: bool) -> Result<Vec<u8>> {
    serde_json::to_vec(&(id, output, suppressed)).map_err(|_| PlatformError("invalid_input"))
}

fn decode_token(encoded: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let mut token = Zeroizing::new(vec![0; 33]);
    if encoded.len() != 43 || URL_SAFE_NO_PAD.decode_slice(encoded, &mut token[..]).ok() != Some(32)
    {
        return Err(PlatformError("source_credential_invalid"));
    }
    token.truncate(32);
    Ok(token)
}

fn confirm_source(name: &str) -> Result<()> {
    if name.trim() != name
        || name.is_empty()
        || name.chars().count() > 60
        || name
            .chars()
            .any(|ch| !(ch.is_alphanumeric() || " ._-()".contains(ch)))
    {
        return Err(PlatformError("invalid_source_name"));
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        let title = crate::windows::wide("AgentNotify - 启用通用来源");
        let prompt = crate::windows::wide(&format!(
            "来源：{name}\n\n启用后，该来源可触发邮件通知，发送给 AgentNotify 已配置的收件人。\n仅在你刚刚主动发起此接入时选择“是”。"
        ));
        let choice = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                prompt.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING | MB_SETFOREGROUND,
            )
        };
        if choice == IDYES {
            Ok(())
        } else {
            Err(PlatformError("source_consent_required"))
        }
    }
    #[cfg(not(windows))]
    Err(PlatformError("source_consent_unavailable"))
}

fn authenticate(key: &hmac::Key, domain: &[u8], payload: &[u8]) -> [u8; 32] {
    hmac::sign(key, &authenticated_input(domain, payload))
        .as_ref()
        .try_into()
        .expect("HMAC SHA-256 length")
}

fn verify(key: &hmac::Key, domain: &[u8], payload: &[u8], tag: &[u8]) -> Result<()> {
    hmac::verify(key, &authenticated_input(domain, payload), tag)
        .map_err(|_| PlatformError("ingress_unauthorized"))
}

fn authenticated_input(domain: &[u8], payload: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut input = Zeroizing::new(Vec::with_capacity(domain.len() + 1 + payload.len()));
    input.extend_from_slice(domain);
    input.push(0);
    input.extend_from_slice(payload);
    input
}

fn source_store_error(error: CredentialError) -> PlatformError {
    PlatformError(if error.code == CredentialErrorCode::Missing {
        "source_not_authorized"
    } else {
        "source_credentials_unavailable"
    })
}
