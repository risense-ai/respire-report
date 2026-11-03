#![allow(dead_code)]
use agentnotify_core::{contracts::*, delivery::smtp::*};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
    pki_types::PrivatePkcs8KeyDer,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
#[derive(Clone, Copy)]
pub enum Scenario {
    Ok,
    Auth,
    Temporary,
    Permanent,
    Disconnect,
    DataTemporary,
    DataPermanent,
    HalfReply,
    NoStartTls,
}
#[derive(Default)]
pub struct Seen {
    pub auth: bool,
    pub data: bool,
    pub body: bool,
    pub durable: bool,
}
pub struct Server {
    pub port: u16,
    pub tls: Arc<ClientConfig>,
    pub seen: Arc<Mutex<Seen>>,
    thread: Option<thread::JoinHandle<()>>,
}
trait Io: Read + Write {}
impl<T: Read + Write> Io for T {}
fn line(s: &mut dyn Io) -> std::io::Result<String> {
    let mut b = Vec::new();
    while !b.ends_with(b"\r\n") {
        if b.len() > 65536 {
            return Err(std::io::Error::other("fixture limit"));
        }
        let mut v = [0];
        s.read_exact(&mut v)?;
        b.push(v[0]);
    }
    String::from_utf8(b).map_err(|_| std::io::Error::other("utf8"))
}
fn send(s: &mut dyn Io, text: &str) -> std::io::Result<()> {
    s.write_all(text.as_bytes())?;
    s.flush()
}
impl Server {
    pub fn start(
        scenario: Scenario,
        starttls: bool,
        durable: Option<Box<dyn Fn() -> bool + Send>>,
    ) -> Self {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(cert.cert.der().clone()).unwrap();
        let tls = Arc::new(
            ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let server = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.cert.der().clone()],
                    PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
                )
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let observed = seen.clone();
        let thread = thread::spawn(move || {
            let run = || -> std::io::Result<()> {
                listener.set_nonblocking(true)?;
                let end = std::time::Instant::now() + Duration::from_secs(6);
                let (mut socket, _) = loop {
                    match listener.accept() {
                        Ok(v) => break v,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < end =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(e) => return Err(e),
                    }
                };
                socket.set_nonblocking(false)?;
                socket.set_read_timeout(Some(Duration::from_secs(5)))?;
                socket.set_write_timeout(Some(Duration::from_secs(5)))?;
                if starttls {
                    send(&mut socket, "220 localhost test\r\n")?;
                    if !line(&mut socket)?.starts_with("EHLO") {
                        return Err(std::io::Error::other("EHLO"));
                    }
                    if matches!(scenario, Scenario::NoStartTls) {
                        send(&mut socket, "250 localhost\r\n")?;
                        if let Ok(command) = line(&mut socket) {
                            assert!(!command.starts_with("AUTH"));
                        }
                        return Ok(());
                    }
                    send(&mut socket, "250-localhost\r\n250 STARTTLS\r\n")?;
                    assert_eq!(line(&mut socket)?, "STARTTLS\r\n");
                    send(&mut socket, "220 upgrade\r\n")?;
                }
                let mut stream = StreamOwned::new(ServerConnection::new(server).unwrap(), socket);
                if !starttls {
                    send(&mut stream, "220 localhost test\r\n")?;
                }
                assert!(line(&mut stream)?.starts_with("EHLO"));
                send(&mut stream, "250-localhost\r\n250 AUTH PLAIN XOAUTH2\r\n")?;
                assert!(line(&mut stream)?.starts_with("AUTH"));
                observed.lock().unwrap().auth = true;
                if matches!(scenario, Scenario::Auth) {
                    send(&mut stream, "535 auth rejected\r\n")?;
                    return Ok(());
                }
                send(&mut stream, "235 authenticated\r\n")?;
                assert!(line(&mut stream)?.starts_with("MAIL FROM"));
                send(&mut stream, "250 sender\r\n")?;
                assert!(line(&mut stream)?.starts_with("RCPT TO"));
                match scenario {
                    Scenario::Temporary => {
                        send(&mut stream, "451 retry later\r\n")?;
                        return Ok(());
                    }
                    Scenario::Permanent => {
                        send(&mut stream, "550 rejected\r\n")?;
                        return Ok(());
                    }
                    _ => {}
                }
                send(&mut stream, "250 recipient\r\n")?;
                let command = match line(&mut stream) {
                    Ok(c) => c,
                    Err(_) => return Ok(()),
                };
                assert_eq!(command, "DATA\r\n");
                {
                    let mut o = observed.lock().unwrap();
                    o.data = true;
                    o.durable = durable.as_ref().map(|f| f()).unwrap_or(true);
                }
                if matches!(scenario, Scenario::Disconnect) {
                    return Ok(());
                }
                send(&mut stream, "354 send content\r\n")?;
                loop {
                    if line(&mut stream)? == ".\r\n" {
                        break;
                    }
                }
                observed.lock().unwrap().body = true;
                match scenario {
                    Scenario::DataTemporary => send(&mut stream, "451 rejected content\r\n")?,
                    Scenario::DataPermanent => send(&mut stream, "554 rejected content\r\n")?,
                    Scenario::HalfReply => send(&mut stream, "451 incomplete")?,
                    _ => send(&mut stream, "250 accepted\r\n")?,
                }
                Ok(())
            };
            let _ = run();
        });
        Self {
            port,
            tls,
            seen,
            thread: Some(thread),
        }
    }
    pub fn finish(mut self) -> Arc<Mutex<Seen>> {
        self.thread.take().unwrap().join().unwrap();
        self.seen.clone()
    }
    pub fn config(&self, starttls: bool) -> SmtpConfig {
        SmtpConfig::new(
            "localhost",
            self.port,
            "sender@example.invalid",
            if starttls { "starttls" } else { "tls" },
        )
        .unwrap()
    }
}
pub struct Journal {
    pub stages: Mutex<Vec<SendStage>>,
    pub fail: Option<SendStage>,
}
impl Journal {
    pub fn new(fail: Option<SendStage>) -> Self {
        Self {
            stages: Mutex::new(vec![]),
            fail,
        }
    }
}
impl SendJournal for Journal {
    fn record(&self, _: &str, s: SendStage) -> Result<(), JournalError> {
        if self.fail == Some(s) {
            return Err(JournalError);
        }
        self.stages.lock().unwrap().push(s);
        Ok(())
    }
    fn rejected(&self, _: &str, _: u16) -> Result<(), JournalError> {
        Ok(())
    }
}
pub fn mail(id: uuid::Uuid) -> PreparedMail {
    PreparedMail {
        message_id: format!("<{id}@agentnotify.local>"),
        from: "sender@example.invalid".into(),
        recipients: vec!["recipient@example.invalid".into()],
        subject: "isolated fixture".into(),
        html_body: None,
        body: SecretInput::new(b"isolated test body\n.\nMAIL FROM: evil\n".to_vec()),
    }
}
pub fn credentials() -> CredentialHandle {
    CredentialHandle::from_secret(SecretInput::new(b"isolated-not-real".to_vec()))
}
