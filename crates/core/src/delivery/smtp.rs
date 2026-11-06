//! 有界SMTP协议；证书验证、显式阶段落盘和网络IO分离，绝不回退明文。
use super::mail::{MailConfig, prepare_queued_mail, wire};
use crate::{
    contracts::*,
    policy::Policy,
    queue::{Lease, SqliteQueue},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned, pki_types::ServerName};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    Implicit,
    StartTls,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpAuth {
    AppPassword,
    OAuth2,
}
#[derive(Clone)]
/// 非秘密SMTP设置；仅接受TLS，用户名不能携带SASL控制字符。
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub tls: TlsMode,
    pub auth: SmtpAuth,
}
impl SmtpConfig {
    pub fn new(host: &str, port: u16, username: &str, tls: &str) -> Result<Self, JournalError> {
        let tls = match tls {
            "tls" => TlsMode::Implicit,
            "starttls" => TlsMode::StartTls,
            _ => return Err(JournalError),
        };
        if host.is_empty()
            || host.len() > 253
            || host.bytes().any(|b| b.is_ascii_control())
            || port == 0
            || !super::mail::valid_address(username)
        {
            return Err(JournalError);
        }
        Ok(Self {
            host: host.into(),
            port,
            username: username.into(),
            tls,
            auth: SmtpAuth::AppPassword,
        })
    }
}
#[derive(Debug)]
pub struct JournalError;
/// 每次send绑定一个租约；成功返回意味着持久提交已完成，不能只是内存标记。
pub trait SendJournal {
    fn record(&self, delivery_id: &str, stage: SendStage) -> Result<(), JournalError>;
    fn rejected(&self, delivery_id: &str, code: u16) -> Result<(), JournalError>;
    /// DATA检查会刷新摘要成员；在提交后重新构建模板，避免使用检查前过期计数。
    fn refresh_mail(&self) -> Result<Option<PreparedMail>, JournalError> {
        Ok(None)
    }
}
pub struct QueueSendJournal<'a> {
    queue: &'a SqliteQueue,
    lease: &'a Lease,
    policy: &'a Policy,
    mail_config: &'a MailConfig,
    stage: Mutex<SendStage>,
}
impl<'a> QueueSendJournal<'a> {
    pub fn new(
        queue: &'a SqliteQueue,
        lease: &'a Lease,
        policy: &'a Policy,
        config: &'a MailConfig,
    ) -> Self {
        Self {
            queue,
            lease,
            policy,
            mail_config: config,
            stage: Mutex::new(lease.send_stage),
        }
    }
}
impl SendJournal for QueueSendJournal<'_> {
    fn record(&self, id: &str, next: SendStage) -> Result<(), JournalError> {
        if id != self.lease.id.to_string() {
            return Err(JournalError);
        }
        let mut stage = self.stage.lock().map_err(|_| JournalError)?;
        self.queue
            .record_send_stage(self.lease, *stage, next)
            .map_err(|_| JournalError)?;
        *stage = next;
        Ok(())
    }
    fn rejected(&self, id: &str, code: u16) -> Result<(), JournalError> {
        if id != self.lease.id.to_string() {
            return Err(JournalError);
        }
        self.queue
            .complete_smtp_rejection(self.lease, code, jitter())
            .map_err(|_| JournalError)
    }
    fn refresh_mail(&self) -> Result<Option<PreparedMail>, JournalError> {
        prepare_queued_mail(self.queue, self.policy, self.lease, self.mail_config)
            .map(Some)
            .map_err(|_| JournalError)
    }
}
/// Mozilla/WebPKI公开根；没有证书忽略开关。测试可注入仅含随机本地CA的配置。
pub fn public_tls() -> Arc<ClientConfig> {
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}
pub struct SmtpTransport<'a> {
    config: SmtpConfig,
    tls: Arc<ClientConfig>,
    journal: &'a dyn SendJournal,
    delivery_id: String,
}
impl<'a> SmtpTransport<'a> {
    pub fn new(
        config: SmtpConfig,
        tls: Arc<ClientConfig>,
        journal: &'a dyn SendJournal,
        delivery_id: uuid::Uuid,
    ) -> Self {
        Self {
            config,
            tls,
            journal,
            delivery_id: delivery_id.to_string(),
        }
    }
    /// 完整SMTP阶段；返回值只含受控码，服务器原文和秘密永不回显。
    fn run(
        &self,
        mail: &PreparedMail,
        credentials: &CredentialHandle,
    ) -> Result<(), DeliveryOutcome> {
        // 1. 公开mail/config再次校验；网络连接尚未建立。
        let mut bytes = wire(mail).map_err(|_| permanent("invalid_mail"))?;
        SmtpConfig::new(
            &self.config.host,
            self.config.port,
            &self.config.username,
            if self.config.tls == TlsMode::Implicit {
                "tls"
            } else {
                "starttls"
            },
        )
        .map_err(|_| permanent("invalid_smtp_config"))?;
        if mail.message_id != format!("<{}@agentnotify.local>", self.delivery_id) {
            return Err(permanent("message_id_mismatch"));
        }
        self.journal
            .record(&self.delivery_id, SendStage::Connecting)
            .map_err(|_| retry("journal_unavailable"))?;
        let deadline = Instant::now() + Duration::from_secs(40);
        let addresses = (self.config.host.as_str(), self.config.port)
            .to_socket_addrs()
            .map_err(|_| retry("dns_failed"))?;
        // 同一主机可能先返回IPv6；有界尝试各地址，不因首个不可达丢弃可用IPv4。
        let mut connected = None;
        for address in addresses.take(8) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Ok(socket) =
                TcpStream::connect_timeout(&address, remaining.min(Duration::from_secs(3)))
            {
                connected = Some(socket);
                break;
            }
        }
        let socket = connected.ok_or_else(|| retry("connect_failed"))?;
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .map_err(|_| retry("socket_failed"))?;
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .map_err(|_| retry("socket_failed"))?;
        let mut stream: Box<dyn Io> =
            Box::new(socket.try_clone().map_err(|_| retry("socket_failed"))?);
        // 2. STARTTLS不允许降级；升级后重新EHLO，之前的能力不用于AUTH。
        if self.config.tls == TlsMode::StartTls {
            expect(&mut *stream, 220, deadline, false)?;
            command(&mut *stream, b"EHLO agentnotify.local\r\n", deadline)?;
            let r = reply(&mut *stream, deadline).map_err(|_| retry("smtp_protocol"))?;
            if r.0 != 250 {
                return Err(classify_reply(r.0, false));
            }
            if !r.1.iter().any(|l| l.eq_ignore_ascii_case("STARTTLS")) {
                return Err(permanent("starttls_required"));
            }
            command(&mut *stream, b"STARTTLS\r\n", deadline)?;
            expect(&mut *stream, 220, deadline, false)?;
        }
        let name = ServerName::try_from(self.config.host.clone())
            .map_err(|_| permanent("invalid_tls_name"))?;
        let connection = ClientConnection::new(self.tls.clone(), name)
            .map_err(|_| permanent("tls_configuration"))?;
        let mut tls = StreamOwned::new(connection, socket);
        while tls.conn.is_handshaking() {
            tls.conn.complete_io(&mut tls.sock).map_err(|e| {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    permanent("tls_validation_failed")
                } else {
                    retry("tls_connection_failed")
                }
            })?;
            if Instant::now() > deadline {
                return Err(retry("tls_timeout"));
            }
        }
        stream = Box::new(tls);
        if self.config.tls == TlsMode::Implicit {
            expect(&mut *stream, 220, deadline, false)?
        }
        command(&mut *stream, b"EHLO agentnotify.local\r\n", deadline)?;
        let (code, caps) = reply(&mut *stream, deadline).map_err(|_| retry("smtp_protocol"))?;
        if code != 250 {
            return Err(classify_reply(code, false));
        }
        let mechanism = if self.config.auth == SmtpAuth::AppPassword {
            "PLAIN"
        } else {
            "XOAUTH2"
        };
        if !caps.iter().any(|l| {
            let mut p = l.split_whitespace();
            p.next().is_some_and(|s| s.eq_ignore_ascii_case("AUTH"))
                && p.any(|s| s.eq_ignore_ascii_case(mechanism))
        }) {
            return Err(DeliveryOutcome::AuthBlocked {
                code: "auth_mechanism_unavailable".into(),
            });
        }
        let auth = credentials.with_secret(|secret| {
            if secret.is_empty()
                || secret
                    .iter()
                    .any(|b| *b == 0 || *b == 1 || *b == b'\r' || *b == b'\n')
            {
                return None;
            }
            let mut raw = zeroize::Zeroizing::new(Vec::new());
            if self.config.auth == SmtpAuth::AppPassword {
                raw.push(0);
                raw.extend(self.config.username.as_bytes());
                raw.push(0);
                raw.extend(secret);
            } else {
                raw.extend(format!("user={}\x01auth=Bearer ", self.config.username).as_bytes());
                raw.extend(secret);
                raw.extend(b"\x01\x01");
            }
            Some(zeroize::Zeroizing::new(format!(
                "AUTH {mechanism} {}\r\n",
                STANDARD.encode(&*raw)
            )))
        });
        let auth = auth.ok_or_else(|| DeliveryOutcome::AuthBlocked {
            code: "credential_invalid".into(),
        })?;
        command(&mut *stream, auth.as_bytes(), deadline)?;
        expect(&mut *stream, 235, deadline, true)?;
        self.journal
            .record(&self.delivery_id, SendStage::Envelope)
            .map_err(|_| retry("journal_unavailable"))?;
        command(
            &mut *stream,
            format!("MAIL FROM:<{}>\r\n", mail.from).as_bytes(),
            deadline,
        )?;
        expect(&mut *stream, 250, deadline, false)?;
        for to in &mail.recipients {
            command(
                &mut *stream,
                format!("RCPT TO:<{to}>\r\n").as_bytes(),
                deadline,
            )?;
            let (code, _) = reply(&mut *stream, deadline).map_err(|_| retry("smtp_protocol"))?;
            if code != 250 && code != 251 {
                return Err(classify_reply(code, false));
            }
        }
        // 3. 落盘失败或策略撤销立即关闭；绝不先写DATA再补记录。
        self.journal
            .record(&self.delivery_id, SendStage::DataStarted)
            .map_err(|_| retry("send_gate_closed"))?;
        let refreshed = self.journal.refresh_mail().map_err(|_| unknown())?;
        if let Some(updated) = &refreshed {
            if updated.message_id != mail.message_id
                || updated.from != mail.from
                || updated.recipients != mail.recipients
            {
                return Err(unknown());
            }
            bytes = wire(updated).map_err(|_| unknown())?;
        }
        command(&mut *stream, b"DATA\r\n", deadline).map_err(|_| unknown())?;
        let (code, _) = reply(&mut *stream, deadline).map_err(|_| unknown())?;
        if code != 354 {
            return self.data_rejected(code);
        }
        command(&mut *stream, &bytes, deadline).map_err(|_| unknown())?;
        let (code, _) = reply(&mut *stream, deadline).map_err(|_| unknown())?;
        if code != 250 {
            return self.data_rejected(code);
        }
        self.journal
            .record(&self.delivery_id, SendStage::Accepted)
            .map_err(|_| unknown())?;
        Ok(())
    }
    fn data_rejected(&self, code: u16) -> Result<(), DeliveryOutcome> {
        if !(400..600).contains(&code) {
            return Err(unknown());
        }
        self.journal
            .rejected(&self.delivery_id, code)
            .map_err(|_| unknown())?;
        Err(classify_reply(code, false))
    }
}
impl Transport for SmtpTransport<'_> {
    fn send(&self, mail: &PreparedMail, credentials: &CredentialHandle) -> DeliveryOutcome {
        match self.run(mail, credentials) {
            Ok(()) => DeliveryOutcome::Accepted {
                message_id: mail.message_id.clone(),
            },
            Err(e) => e,
        }
    }
}
trait Io: Read + Write {}
impl<T: Read + Write> Io for T {}
fn command(s: &mut dyn Io, b: &[u8], end: Instant) -> Result<(), DeliveryOutcome> {
    if Instant::now() > end {
        return Err(retry("smtp_timeout"));
    }
    s.write_all(b)
        .and_then(|_| s.flush())
        .map_err(|_| retry("smtp_io"))
}
/// 严格完整CRLF、多行同一状态码，限制每行2048字节/64行；半响应不能证明未发送。
fn reply(s: &mut dyn Io, end: Instant) -> Result<(u16, Vec<String>), ()> {
    let mut code = None;
    let mut lines = Vec::new();
    for _ in 0..64 {
        let mut bytes = Vec::new();
        loop {
            if bytes.len() >= 2048 || Instant::now() > end {
                return Err(());
            }
            let mut b = [0];
            s.read_exact(&mut b).map_err(|_| ())?;
            bytes.push(b[0]);
            if b[0] == b'\n' {
                break;
            }
        }
        if bytes.len() < 6
            || !bytes.ends_with(b"\r\n")
            || !bytes[..3].iter().all(u8::is_ascii_digit)
            || !matches!(bytes[3], b' ' | b'-')
        {
            return Err(());
        }
        let n = std::str::from_utf8(&bytes[..3])
            .map_err(|_| ())?
            .parse::<u16>()
            .map_err(|_| ())?;
        if code.is_some_and(|v| v != n) {
            return Err(());
        }
        code = Some(n);
        lines.push(String::from_utf8(bytes[4..bytes.len() - 2].to_vec()).map_err(|_| ())?);
        if bytes[3] == b' ' {
            return Ok((n, lines));
        }
    }
    Err(())
}
fn expect(s: &mut dyn Io, want: u16, end: Instant, auth: bool) -> Result<(), DeliveryOutcome> {
    let (code, _) = reply(s, end).map_err(|_| retry("smtp_protocol"))?;
    if code == want {
        Ok(())
    } else {
        Err(classify_reply(code, auth))
    }
}
pub fn classify_reply(code: u16, auth: bool) -> DeliveryOutcome {
    if auth && ((500..600).contains(&code) || code == 334) {
        DeliveryOutcome::AuthBlocked {
            code: "smtp_auth_rejected".into(),
        }
    } else if (400..500).contains(&code) {
        retry("smtp_temporary_rejection")
    } else if (500..600).contains(&code) {
        permanent("smtp_permanent_rejection")
    } else {
        permanent("smtp_unexpected_reply")
    }
}
fn retry(c: &str) -> DeliveryOutcome {
    DeliveryOutcome::Retryable { code: c.into() }
}
fn permanent(c: &str) -> DeliveryOutcome {
    DeliveryOutcome::PermanentFailure { code: c.into() }
}
fn unknown() -> DeliveryOutcome {
    DeliveryOutcome::Uncertain {
        stage: SendStage::DataStarted,
    }
}
fn jitter() -> u8 {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut b = [0];
    if SystemRandom::new().fill(&mut b).is_ok() {
        b[0] % 21
    } else {
        0
    }
}
/// 调度器的唯一投递入口：安全取密失败auth_blocked；已明确拒收由journal原子完成，不能再次转换。
pub fn deliver_lease(
    queue: &SqliteQueue,
    policy: &Policy,
    lease: &Lease,
    config: &MailConfig,
    smtp: SmtpConfig,
    store: &dyn CredentialStore,
    reference: &CredentialRef,
    clock: &dyn crate::clock::Clock,
) -> Result<DeliveryOutcome, QueueError> {
    // 已撤销或旧版从未授权的来源保留历史状态，不建立 SMTP 连接。
    match queue.lease_source_authorized(lease) {
        Ok(true) => {}
        Ok(false) => {
            queue.complete_lease(lease, QueueStatus::PermanentFailed)?;
            return Ok(permanent("source_not_authorized"));
        }
        Err(_) => {
            queue.retry_temporary(lease, clock.now(), jitter())?;
            return Ok(retry("source_verification_unavailable"));
        }
    }
    let credential_result = if smtp.auth == SmtpAuth::OAuth2 {
        super::oauth::access_token(
            store,
            reference,
            &super::oauth::HttpsTokenEndpoint::default(),
            clock.now().timestamp(),
        )
        .map_err(|_| CredentialError {
            code: CredentialErrorCode::Unavailable,
        })
    } else {
        store.get(reference)
    };
    let credentials = match credential_result {
        Ok(c) => c,
        Err(_) => {
            queue.complete_lease(lease, QueueStatus::AuthBlocked)?;
            return Ok(DeliveryOutcome::AuthBlocked {
                code: "secure_credential_unavailable".into(),
            });
        }
    };
    let outcome = match prepare_queued_mail(queue, policy, lease, config) {
        Ok(mail) => {
            let journal = QueueSendJournal::new(queue, lease, policy, config);
            SmtpTransport::new(smtp, public_tls(), &journal, lease.id).send(&mail, &credentials)
        }
        // DATA尚未开始；存储错误不能当成非法邮件清正文，沿用有界重试与TTL。
        Err(super::mail::MailError::Queue(e)) => match e.code {
            QueueErrorCode::BodyProtectionUnavailable => DeliveryOutcome::AuthBlocked {
                code: "content_key_unavailable".into(),
            },
            QueueErrorCode::UnsupportedSchema => DeliveryOutcome::AuthBlocked {
                code: "queue_schema_unavailable".into(),
            },
            _ => retry("mail_storage_unavailable"),
        },
        // 取密/解密能力与配置可修复，等待用户处理期间保留尚未到TTL的加密正文。
        Err(super::mail::MailError::ProtectionUnavailable) => DeliveryOutcome::AuthBlocked {
            code: "content_key_unavailable".into(),
        },
        Err(super::mail::MailError::ConfigurationUnavailable) => DeliveryOutcome::AuthBlocked {
            code: "mail_configuration_unavailable".into(),
        },
        Err(super::mail::MailError::InvalidMail(code)) => permanent(code),
    };
    if queue.notification_status(lease.id)? == QueueStatus::Leased {
        match &outcome {
            DeliveryOutcome::Retryable { .. } => {
                queue.retry_temporary(lease, clock.now(), jitter())?
            }
            DeliveryOutcome::Accepted { .. } => queue.complete_lease(lease, QueueStatus::Sent)?,
            DeliveryOutcome::AuthBlocked { .. } => {
                queue.complete_lease(lease, QueueStatus::AuthBlocked)?
            }
            DeliveryOutcome::PermanentFailure { .. } => {
                queue.complete_lease(lease, QueueStatus::PermanentFailed)?
            }
            DeliveryOutcome::Uncertain { .. } => {
                queue.complete_lease(lease, QueueStatus::DeliveryUnknown)?
            }
        }
    }
    Ok(outcome)
}
