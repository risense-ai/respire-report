//! 公共桌面OAuth：授权码+PKCE、短生命本地事务；不消费ID token，因此不存在access-token nonce验证。
use crate::{clock::Clock, contracts::*};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::{SecureRandom, SystemRandom},
    signature,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    time::Duration,
};
use url::Url;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthError {
    Unavailable,
    InvalidCallback,
    Expired,
    Denied,
    ExchangeFailed,
    RefreshFailed,
    Revoked,
    SecureStorage,
    InvalidProvider,
    InvalidSignature,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// 仅来自发布者受信配置；不是让普通用户注册应用。无client secret，无openid scope。
pub struct OAuthProvider {
    pub client_id: String,
    pub authorization_url: String,
    pub token_url: String,
    pub revocation_url: Option<String>,
    pub scopes: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub id: String,
    pub smtp_host: String,
    pub port: u16,
    pub tls: String,
    pub availability: String,
    pub condition: String,
    pub oauth: Option<OAuthProvider>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCatalog {
    pub version: u64,
    pub providers: Vec<Provider>,
}
impl ProviderCatalog {
    pub fn bundled() -> Result<Self, OAuthError> {
        serde_json::from_str(include_str!(
            "../../../../resources/providers/providers-v1.json"
        ))
        .map_err(|_| OAuthError::InvalidProvider)
    }
    pub fn oauth_available(&self, id: &str) -> bool {
        self.providers.iter().any(|p| {
            p.id == id
                && p.availability != "unavailable"
                && p.oauth.as_ref().is_some_and(|o| o.validate().is_ok())
        })
    }
    /// 运行时更新：发布者信任根尚未交付，因此正式路径明确不可用，不能把测试key嵌入产品。
    pub fn install_update(
        bytes: &[u8],
        signature: &[u8],
        current: u64,
    ) -> Result<Self, OAuthError> {
        Self::verify_update(bytes, signature, None, current)
    }
    /// 验签机制独立于信任根交付；key只能来自受信发布配置，不能取自下载内容。
    pub fn verify_update(
        bytes: &[u8],
        sig: &[u8],
        publisher_key: Option<&[u8]>,
        current: u64,
    ) -> Result<Self, OAuthError> {
        let key = publisher_key.ok_or(OAuthError::Unavailable)?;
        if bytes.len() > 65536 {
            return Err(OAuthError::InvalidProvider);
        }
        signature::UnparsedPublicKey::new(&signature::ED25519, key)
            .verify(bytes, sig)
            .map_err(|_| OAuthError::InvalidSignature)?;
        let catalog: Self =
            serde_json::from_slice(bytes).map_err(|_| OAuthError::InvalidProvider)?;
        if catalog.version <= current || catalog.providers.len() > 32 {
            return Err(OAuthError::InvalidProvider);
        }
        for p in &catalog.providers {
            if p.id.is_empty() || !matches!(p.tls.as_str(), "tls" | "starttls") || p.port == 0 {
                return Err(OAuthError::InvalidProvider);
            }
            if let Some(o) = &p.oauth {
                o.validate()?
            }
        }
        Ok(catalog)
    }
}
fn https(raw: &str) -> Result<Url, OAuthError> {
    let u = Url::parse(raw).map_err(|_| OAuthError::InvalidProvider)?;
    if u.scheme() != "https"
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        return Err(OAuthError::InvalidProvider);
    }
    Ok(u)
}
impl OAuthProvider {
    pub fn validate(&self) -> Result<(), OAuthError> {
        if self.client_id.trim().is_empty()
            || self.client_id.len() > 512
            || self.scopes.is_empty()
            || self
                .scopes
                .iter()
                .any(|s| s == "openid" || s.contains(char::is_whitespace))
        {
            return Err(OAuthError::Unavailable);
        }
        https(&self.authorization_url)?;
        https(&self.token_url)?;
        if let Some(u) = &self.revocation_url {
            https(u)?;
        }
        Ok(())
    }
}
/// 不包含token或PKCE verifier，供系统浏览器打开；session_id用于本机一次性事务匹配。
pub struct AuthorizationRequest {
    pub url: String,
    pub session_id: uuid::Uuid,
}
/// 由本地listener解析；state和code只短暂保存在内存，不实现Debug/Serialize。
pub struct OAuthCallback {
    pub session_id: uuid::Uuid,
    pub redirect_uri: String,
    pub state: String,
    pub code: Option<SecretInput>,
    pub error: Option<String>,
}
struct Pending {
    id: uuid::Uuid,
    state: zeroize::Zeroizing<String>,
    verifier: SecretInput,
    redirect: String,
    started: Duration,
    listener: TcpListener,
}
pub struct OAuthSession<'a> {
    provider: OAuthProvider,
    clock: Arc<dyn Clock>,
    store: &'a dyn CredentialStore,
    endpoint: &'a dyn TokenEndpoint,
    pending: Option<Pending>,
}
/// token仅在内存和系统安全存储中；不derive Debug，序列化只允许进入CredentialStore。
#[derive(Serialize, Deserialize)]
struct StoredTokens {
    provider: OAuthProvider,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: i64,
}
impl Drop for StoredTokens {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.access_token.zeroize();
        if let Some(t) = &mut self.refresh_token {
            t.zeroize()
        }
    }
}
pub struct TokenSet {
    pub access_token: SecretInput,
    pub refresh_token: Option<SecretInput>,
    pub expires_in: u64,
}
/// 交换层接受绑定会话的verifier；测试实现不用于生产回退。
pub trait TokenEndpoint {
    fn exchange(
        &self,
        p: &OAuthProvider,
        code: &SecretInput,
        verifier: &SecretInput,
        redirect: &str,
    ) -> Result<TokenSet, OAuthError>;
    fn refresh(&self, p: &OAuthProvider, refresh: &SecretInput) -> Result<TokenSet, OAuthError>;
    fn revoke(&self, p: &OAuthProvider, token: &SecretInput) -> Result<(), OAuthError>;
}
pub struct HttpsTokenEndpoint {
    agent: ureq::Agent,
}
impl Default for HttpsTokenEndpoint {
    fn default() -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .https_only(true)
                .redirects(0)
                .timeout(Duration::from_secs(15))
                .build(),
        }
    }
}
impl HttpsTokenEndpoint {
    /// 由受信宿主注入TLS根（隔离测试用随机CA）；生产默认使用公开根，禁止关闭验证。
    pub fn with_tls(config: Arc<rustls::ClientConfig>) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .https_only(true)
                .redirects(0)
                .timeout(Duration::from_secs(15))
                .tls_config(config)
                .build(),
        }
    }

    /// HTTPS、无重定向、有界响应；底层错误原文可能含token，统一转换为受控码。
    fn request(&self, url: &str, fields: &[(&str, &str)]) -> Result<TokenSet, OAuthError> {
        https(url)?;
        let response = self
            .agent
            .post(url)
            .send_form(fields)
            .map_err(|_| OAuthError::ExchangeFailed)?;
        let mut raw = zeroize::Zeroizing::new(Vec::new());
        response
            .into_reader()
            .take(65537)
            .read_to_end(&mut raw)
            .map_err(|_| OAuthError::ExchangeFailed)?;
        if raw.len() > 65536 {
            return Err(OAuthError::ExchangeFailed);
        }
        #[derive(Deserialize)]
        struct Response {
            access_token: String,
            refresh_token: Option<String>,
            expires_in: u64,
            token_type: String,
        }
        let mut r: Response =
            serde_json::from_slice(&raw).map_err(|_| OAuthError::ExchangeFailed)?;
        if !r.token_type.eq_ignore_ascii_case("Bearer")
            || r.access_token.is_empty()
            || r.expires_in == 0
            || r.expires_in > 86400
        {
            use zeroize::Zeroize;
            r.access_token.zeroize();
            if let Some(v) = &mut r.refresh_token {
                v.zeroize()
            }
            return Err(OAuthError::ExchangeFailed);
        }
        Ok(TokenSet {
            access_token: SecretInput::new(r.access_token.into_bytes()),
            refresh_token: r.refresh_token.map(|s| SecretInput::new(s.into_bytes())),
            expires_in: r.expires_in,
        })
    }
}
impl TokenEndpoint for HttpsTokenEndpoint {
    fn exchange(
        &self,
        p: &OAuthProvider,
        code: &SecretInput,
        verifier: &SecretInput,
        redirect: &str,
    ) -> Result<TokenSet, OAuthError> {
        p.validate()?;
        code.with_secret(|c| {
            verifier.with_secret(|v| {
                self.request(
                    &p.token_url,
                    &[
                        ("grant_type", "authorization_code"),
                        ("client_id", &p.client_id),
                        (
                            "code",
                            std::str::from_utf8(c).map_err(|_| OAuthError::ExchangeFailed)?,
                        ),
                        (
                            "code_verifier",
                            std::str::from_utf8(v).map_err(|_| OAuthError::ExchangeFailed)?,
                        ),
                        ("redirect_uri", redirect),
                    ],
                )
            })
        })
    }
    fn refresh(&self, p: &OAuthProvider, refresh: &SecretInput) -> Result<TokenSet, OAuthError> {
        p.validate()?;
        refresh
            .with_secret(|r| {
                self.request(
                    &p.token_url,
                    &[
                        ("grant_type", "refresh_token"),
                        ("client_id", &p.client_id),
                        (
                            "refresh_token",
                            std::str::from_utf8(r).map_err(|_| OAuthError::RefreshFailed)?,
                        ),
                        ("scope", &p.scopes.join(" ")),
                    ],
                )
            })
            .map_err(|_| OAuthError::RefreshFailed)
    }
    fn revoke(&self, p: &OAuthProvider, token: &SecretInput) -> Result<(), OAuthError> {
        let url = p.revocation_url.as_ref().ok_or(OAuthError::Unavailable)?;
        https(url)?;
        token.with_secret(|t| {
            self.agent
                .post(url)
                .send_form(&[
                    (
                        "token",
                        std::str::from_utf8(t).map_err(|_| OAuthError::ExchangeFailed)?,
                    ),
                    ("client_id", &p.client_id),
                ])
                .map(|_| ())
                .map_err(|_| OAuthError::ExchangeFailed)
        })
    }
}
fn random() -> Result<String, OAuthError> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| OAuthError::Unavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}
impl<'a> OAuthSession<'a> {
    pub fn new(
        provider: OAuthProvider,
        clock: Arc<dyn Clock>,
        store: &'a dyn CredentialStore,
        endpoint: &'a dyn TokenEndpoint,
    ) -> Result<Self, OAuthError> {
        provider.validate()?;
        Ok(Self {
            provider,
            clock,
            store,
            endpoint,
            pending: None,
        })
    }
    pub fn start(&mut self) -> Result<AuthorizationRequest, OAuthError> {
        // 1. 监听只绑定IPv4回环，随机端口/路径，旧会话替换即关闭，不监听外部接口。
        self.provider.validate()?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|_| OAuthError::Unavailable)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| OAuthError::Unavailable)?;
        let id = uuid::Uuid::new_v4();
        let redirect = format!(
            "http://127.0.0.1:{}/oauth/{id}",
            listener
                .local_addr()
                .map_err(|_| OAuthError::Unavailable)?
                .port()
        );
        let state = zeroize::Zeroizing::new(random()?);
        let verifier = SecretInput::new(random()?.into_bytes());
        let challenge = verifier.with_secret(|v| URL_SAFE_NO_PAD.encode(Sha256::digest(v)));
        // 2. OAuth state关联事务，PKCE挑战绑定授权码；未请求openid，不请求/消费OIDC nonce。
        let mut url = https(&self.provider.authorization_url)?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", &self.provider.client_id),
            ("redirect_uri", &redirect),
            ("scope", &self.provider.scopes.join(" ")),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        self.pending = Some(Pending {
            id,
            state,
            verifier,
            redirect,
            started: self.clock.monotonic(),
            listener,
        });
        Ok(AuthorizationRequest {
            url: url.into(),
            session_id: id,
        })
    }
    /// 非阻塞轮询供宿主事件循环调用；只读取一个有限GET请求，回应不包含code/token。
    pub fn poll_callback(&mut self) -> Result<Option<OAuthCallback>, OAuthError> {
        let p = self.pending.as_ref().ok_or(OAuthError::InvalidCallback)?;
        if self.clock.monotonic().saturating_sub(p.started) >= Duration::from_secs(300) {
            self.pending = None;
            return Err(OAuthError::Expired);
        }
        let (mut socket, remote) = match p.listener.accept() {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(_) => return Err(OAuthError::InvalidCallback),
        };
        if !remote.ip().is_loopback() {
            return Err(OAuthError::InvalidCallback);
        }
        socket
            .set_nonblocking(false)
            .map_err(|_| OAuthError::InvalidCallback)?;
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| OAuthError::InvalidCallback)?;
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| OAuthError::InvalidCallback)?;
        let mut bytes = zeroize::Zeroizing::new(Vec::new());
        while !bytes.ends_with(b"\r\n\r\n") {
            if bytes.len() >= 8192 {
                return Err(OAuthError::InvalidCallback);
            }
            let mut b = [0];
            socket
                .read_exact(&mut b)
                .map_err(|_| OAuthError::InvalidCallback)?;
            bytes.push(b[0]);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| OAuthError::InvalidCallback)?;
        let line = text.lines().next().ok_or(OAuthError::InvalidCallback)?;
        let mut parts = line.split_whitespace();
        if parts.next() != Some("GET") {
            return Err(OAuthError::InvalidCallback);
        }
        let path = parts.next().ok_or(OAuthError::InvalidCallback)?;
        if parts.next() != Some("HTTP/1.1")
            || parts.next().is_some()
            || !path.starts_with('/')
            || path.starts_with("//")
        {
            return Err(OAuthError::InvalidCallback);
        }
        let expected = Url::parse(&p.redirect).map_err(|_| OAuthError::InvalidCallback)?;
        let authority = format!("127.0.0.1:{}", expected.port().unwrap());
        let hosts: Vec<_> = text
            .lines()
            .filter_map(|l| l.split_once(':'))
            .filter(|(k, _)| k.eq_ignore_ascii_case("Host"))
            .collect();
        if hosts.len() != 1 || hosts[0].1.trim() != authority {
            return Err(OAuthError::InvalidCallback);
        }
        let url = expected
            .join(path)
            .map_err(|_| OAuthError::InvalidCallback)?;
        if url.path() != expected.path() {
            return Err(OAuthError::InvalidCallback);
        }
        let mut params = std::collections::HashMap::new();
        for (k, v) in url.query_pairs() {
            if params.insert(k.into_owned(), v.into_owned()).is_some() {
                return Err(OAuthError::InvalidCallback);
            }
        }
        let callback = OAuthCallback {
            session_id: p.id,
            redirect_uri: p.redirect.clone(),
            state: params.remove("state").ok_or(OAuthError::InvalidCallback)?,
            code: params
                .remove("code")
                .map(|s| SecretInput::new(s.into_bytes())),
            error: params.remove("error"),
        };
        let _=socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 34\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nReturn to AgentNotify to continue.\n");
        Ok(Some(callback))
    }
    pub fn finish(&mut self, callback: OAuthCallback) -> Result<CredentialRef, OAuthError> {
        // 1. 先消费事务，成功/失败/拒绝均不可重放；不得错误回调后保持可重复授权。
        let p = self.pending.take().ok_or(OAuthError::InvalidCallback)?;
        if self.clock.monotonic().saturating_sub(p.started) >= Duration::from_secs(300) {
            return Err(OAuthError::Expired);
        }
        if callback.session_id != p.id
            || callback.redirect_uri != p.redirect
            || !same(&callback.state, &p.state)
        {
            return Err(OAuthError::InvalidCallback);
        }
        if callback.error.is_some() {
            return Err(OAuthError::Denied);
        }
        let code = callback.code.ok_or(OAuthError::InvalidCallback)?;
        // 2. 网络交换不持有队列事务，成功token只写系统安全存储；失败不返回临时token。
        let tokens = self
            .endpoint
            .exchange(&self.provider, &code, &p.verifier, &p.redirect)?;
        let r = CredentialRef {
            id: uuid::Uuid::new_v4(),
        };
        save_tokens(
            self.store,
            &r,
            &self.provider,
            tokens,
            None,
            self.clock.now().timestamp(),
            None,
        )?;
        Ok(r)
    }
}
fn save_tokens(
    store: &dyn CredentialStore,
    r: &CredentialRef,
    p: &OAuthProvider,
    t: TokenSet,
    old_refresh: Option<String>,
    now: i64,
    expected: Option<&CredentialRevision>,
) -> Result<(), OAuthError> {
    let access = t
        .access_token
        .with_secret(|v| std::str::from_utf8(v).map(str::to_owned))
        .map_err(|_| OAuthError::ExchangeFailed)?;
    let refresh = match t.refresh_token {
        Some(v) => Some(
            v.with_secret(|b| std::str::from_utf8(b).map(str::to_owned))
                .map_err(|_| OAuthError::ExchangeFailed)?,
        ),
        None => old_refresh,
    };
    let stored = StoredTokens {
        provider: p.clone(),
        access_token: access,
        refresh_token: refresh,
        expires_at: now + t.expires_in.min(86400) as i64,
    };
    let bytes = serde_json::to_vec(&stored).map_err(|_| OAuthError::SecureStorage)?;
    match expected {
        // 首次授权仅使用finish新生成的随机引用，尚未向宿主发布。
        None => store
            .put(r, SecretInput::new(bytes))
            .map_err(|_| OAuthError::SecureStorage),
        Some(revision) => {
            // HTTPS期间可能发生撤销或另一次刷新；旧快照不能无条件覆盖/重新创建凭据。
            if store
                .replace_if_revision(r, revision, SecretInput::new(bytes))
                .map_err(|_| OAuthError::SecureStorage)?
            {
                Ok(())
            } else {
                Err(OAuthError::Revoked)
            }
        }
    }
}
/// 使用真实HTTPS刷新生产凭据；刷新失败停发，不沿用过期token，不转换成账号密码。
pub fn access_token(
    store: &dyn CredentialStore,
    r: &CredentialRef,
    endpoint: &dyn TokenEndpoint,
    now: i64,
) -> Result<CredentialHandle, OAuthError> {
    let handle = store.get(r).map_err(|_| OAuthError::SecureStorage)?;
    let mut tokens: StoredTokens = handle
        .with_secret(|b| serde_json::from_slice(b))
        .map_err(|_| OAuthError::SecureStorage)?;
    if tokens.expires_at <= now + 60 {
        // 无CAS能力的后端不能安全刷新；取到版本后立即释放原生读取锁，网络不持锁。
        let revision = handle.revision().ok_or(OAuthError::SecureStorage)?;
        let refresh = tokens.refresh_token.as_ref().ok_or(OAuthError::Revoked)?;
        let next = endpoint
            .refresh(
                &tokens.provider,
                &SecretInput::new(refresh.as_bytes().to_vec()),
            )
            .map_err(|_| OAuthError::RefreshFailed)?;
        save_tokens(
            store,
            r,
            &tokens.provider,
            next,
            tokens.refresh_token.take(),
            now,
            Some(revision),
        )?;
        return stored_access(store, r);
    }
    Ok(CredentialHandle::from_secret(SecretInput::new(
        tokens.access_token.as_bytes().to_vec(),
    )))
}
fn stored_access(
    store: &dyn CredentialStore,
    r: &CredentialRef,
) -> Result<CredentialHandle, OAuthError> {
    let handle = store.get(r).map_err(|_| OAuthError::SecureStorage)?;
    let tokens: StoredTokens = handle
        .with_secret(|b| serde_json::from_slice(b))
        .map_err(|_| OAuthError::SecureStorage)?;
    Ok(CredentialHandle::from_secret(SecretInput::new(
        tokens.access_token.as_bytes().to_vec(),
    )))
}
/// 先完成本地删除使所有旧刷新快照失效，再尝试HTTPS撤销；远端失败不恢复本地授权。
/// 已发出的网络请求/已借出的内存句柄不能撤回，但不会据其结果重新创建凭据。
pub fn revoke(
    store: &dyn CredentialStore,
    r: &CredentialRef,
    endpoint: &dyn TokenEndpoint,
) -> Result<(), OAuthError> {
    let handle = store.get(r).map_err(|_| OAuthError::SecureStorage)?;
    // 删除与条件刷新共用原生临界区；删除失败不宣称已断开，也不开始远端请求。
    store.delete(r).map_err(|_| OAuthError::SecureStorage)?;
    let tokens: StoredTokens = handle
        .with_secret(|b| serde_json::from_slice(b))
        .map_err(|_| OAuthError::SecureStorage)?;
    let result = endpoint.revoke(
        &tokens.provider,
        &SecretInput::new(
            tokens
                .refresh_token
                .as_ref()
                .unwrap_or(&tokens.access_token)
                .as_bytes()
                .to_vec(),
        ),
    );
    result
}
