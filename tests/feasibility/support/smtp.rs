use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair, KeyUsagePurpose};
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName},
};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

#[derive(Clone, Copy)]
enum Scenario {
    Success,
    NoStartTls,
    Untrusted,
    DataDisconnect,
    MarkerUnavailable,
}

struct CertificateFixture {
    root: CertificateDer<'static>,
    server: Arc<ServerConfig>,
}

/// Generate a unique in-memory CA and leaf; never install the CA into a system trust store.
fn certificates() -> Result<CertificateFixture, String> {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).map_err(|_| "CA params")?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().map_err(|_| "CA key")?)
        .map_err(|_| "CA certificate")?;
    let key = KeyPair::generate().map_err(|_| "leaf key")?;
    let params = CertificateParams::new(vec!["localhost".into()]).map_err(|_| "leaf params")?;
    let leaf = params.signed_by(&key, &ca).map_err(|_| "leaf signing")?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .map_err(|_| "server TLS config")?;
    Ok(CertificateFixture {
        root: ca.der().clone(),
        server: Arc::new(config),
    })
}

/// Bounded line IO avoids prefetching plaintext bytes across the STARTTLS transition.
fn line(stream: &mut (impl Read + ?Sized)) -> io::Result<String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() > 2048 {
            return Err(io::Error::other("oversize SMTP line"));
        }
    }
    String::from_utf8(bytes).map_err(|_| io::Error::other("SMTP encoding"))
}

fn send(stream: &mut (impl Write + ?Sized), text: &str) -> io::Result<()> {
    stream.write_all(text.as_bytes())?;
    stream.flush()
}

fn expect(stream: &mut (impl Read + ?Sized), prefix: &str) -> io::Result<()> {
    if !line(stream)?.starts_with(prefix) {
        return Err(io::Error::other("unexpected SMTP stage"));
    }
    Ok(())
}

struct Observation {
    saw_data: bool,
    durable_before_data: bool,
    marker_after_result: bool,
}

/// A loopback-only SMTP fixture. Every accepted connection has bounded IO timeouts.
fn server(
    listener: TcpListener,
    config: Arc<ServerConfig>,
    scenario: Scenario,
    marker: PathBuf,
) -> Result<Observation, String> {
    listener
        .set_nonblocking(true)
        .map_err(|_| "fixture listener mode")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    let (mut socket, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err("fixture accept timeout".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return Err("fixture accept".into()),
        }
    };
    // Windows accepted sockets inherit the listener mode; switch only this stream back to bounded blocking IO.
    socket
        .set_nonblocking(false)
        .map_err(|_| "fixture stream mode")?;
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .map_err(|_| "read timeout")?;
    socket
        .set_write_timeout(Some(Duration::from_secs(4)))
        .map_err(|_| "write timeout")?;
    send(&mut socket, "220 localhost M0\r\n").map_err(|_| "banner")?;
    expect(&mut socket, "EHLO ").map_err(|error| format!("EHLO io kind={:?}", error.kind()))?;
    if matches!(scenario, Scenario::NoStartTls) {
        send(&mut socket, "250 localhost\r\n").map_err(|_| "downgrade fixture")?;
        // Client must close without AUTH, MAIL, RCPT or DATA.
        if line(&mut socket).is_ok() {
            return Err("plaintext command after STARTTLS missing".into());
        }
        return Ok(Observation {
            saw_data: false,
            durable_before_data: false,
            marker_after_result: false,
        });
    }
    send(&mut socket, "250-localhost\r\n250 STARTTLS\r\n").map_err(|_| "STARTTLS capability")?;
    expect(&mut socket, "STARTTLS").map_err(|_| "STARTTLS command")?;
    send(&mut socket, "220 Ready for TLS\r\n").map_err(|_| "TLS readiness")?;
    let connection = ServerConnection::new(config).map_err(|_| "server TLS connection")?;
    let mut tls = StreamOwned::new(connection, socket);
    if expect(&mut tls, "EHLO ").is_err() {
        if matches!(scenario, Scenario::Untrusted) {
            return Ok(Observation {
                saw_data: false,
                durable_before_data: false,
                marker_after_result: false,
            });
        }
        return Err("TLS/EHLO handshake failed".into());
    }
    send(&mut tls, "250 localhost\r\n").map_err(|_| "secure EHLO response")?;
    expect(&mut tls, "MAIL FROM:").map_err(|_| "MAIL")?;
    send(&mut tls, "250 sender ok\r\n").map_err(|_| "MAIL response")?;
    expect(&mut tls, "RCPT TO:").map_err(|_| "RCPT")?;
    send(&mut tls, "250 recipient ok\r\n").map_err(|_| "RCPT response")?;
    let command = match line(&mut tls) {
        Ok(value) => value,
        Err(_) if matches!(scenario, Scenario::MarkerUnavailable) => {
            return Ok(Observation {
                saw_data: false,
                durable_before_data: false,
                marker_after_result: false,
            });
        }
        Err(_) => return Err("DATA command missing".into()),
    };
    if command != "DATA\r\n" {
        return Err("unexpected post-envelope command".into());
    }
    // Read the synced file at the receiver before acknowledging DATA.
    let durable = fs::read(&marker)
        .map(|bytes| bytes == b"data_started\n")
        .unwrap_or(false);
    if !durable {
        return Err("DATA arrived before durable data_started".into());
    }
    send(&mut tls, "354 continue\r\n").map_err(|_| "DATA response")?;
    let mut lines = 0;
    while line(&mut tls).map_err(|_| "test message")? != ".\r\n" {
        lines += 1;
        if lines > 32 {
            return Err("test message exceeded bound".into());
        }
    }
    // Deliberately disconnect after the complete message to create an ambiguous result.
    if !matches!(scenario, Scenario::DataDisconnect) {
        send(&mut tls, "250 accepted locally\r\n").map_err(|_| "accept response")?;
    }
    Ok(Observation {
        saw_data: true,
        durable_before_data: durable,
        marker_after_result: false,
    })
}

enum Outcome {
    Accepted,
    TlsRejected,
    DowngradeRejected,
    MarkerRejected,
    DeliveryUncertain,
}

/// The auditable experiment controls the exact transition. No high-level send() guesses.
fn client(
    address: SocketAddr,
    root: CertificateDer<'static>,
    scenario: Scenario,
    marker: &Path,
) -> Result<Outcome, String> {
    // 1. Fail closed if STARTTLS is unavailable; no credentials are used in this experiment.
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(4))
        .map_err(|_| "loopback connect")?;
    // Explicit blocking IO uses the bounded timeouts below.
    socket
        .set_nonblocking(false)
        .map_err(|_| "fixture stream mode")?;
    socket
        .set_read_timeout(Some(Duration::from_secs(4)))
        .map_err(|_| "client timeout")?;
    socket
        .set_write_timeout(Some(Duration::from_secs(4)))
        .map_err(|_| "client timeout")?;
    expect(&mut socket, "220").map_err(|_| "SMTP banner")?;
    send(&mut socket, "EHLO localhost\r\n").map_err(|_| "client EHLO")?;
    let mut starttls = false;
    for _ in 0..16 {
        let capability = line(&mut socket).map_err(|_| "capabilities")?;
        starttls |= capability[4.min(capability.len())..]
            .trim()
            .eq_ignore_ascii_case("STARTTLS");
        if capability.starts_with("250 ") {
            break;
        }
    }
    if !starttls {
        return Ok(Outcome::DowngradeRejected);
    }
    send(&mut socket, "STARTTLS\r\n").map_err(|_| "client STARTTLS")?;
    expect(&mut socket, "220").map_err(|_| "server TLS response")?;
    let mut roots = RootCertStore::empty();
    if !matches!(scenario, Scenario::Untrusted) {
        roots.add(root).map_err(|_| "test root")?;
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connection =
        ClientConnection::new(Arc::new(config), ServerName::try_from("localhost").unwrap())
            .map_err(|_| "client TLS")?;
    let mut tls = StreamOwned::new(connection, socket);
    if send(&mut tls, "EHLO localhost\r\n").is_err() || expect(&mut tls, "250").is_err() {
        return Ok(Outcome::TlsRejected);
    }
    // 2. Use only reserved .invalid addresses and a synthetic message on loopback.
    send(&mut tls, "MAIL FROM:<probe@localhost.invalid>\r\n").map_err(|_| "client MAIL")?;
    expect(&mut tls, "250").map_err(|_| "MAIL accepted")?;
    send(&mut tls, "RCPT TO:<sink@localhost.invalid>\r\n").map_err(|_| "client RCPT")?;
    expect(&mut tls, "250").map_err(|_| "RCPT accepted")?;
    // 3. Persist + sync BEFORE DATA; if persistence fails, do not send DATA or body.
    let persisted = File::create(marker).and_then(|mut file| {
        file.write_all(b"data_started\n")?;
        file.sync_all()
    });
    if persisted.is_err() {
        return Ok(Outcome::MarkerRejected);
    }
    send(&mut tls, "DATA\r\n").map_err(|_| "client DATA")?;
    expect(&mut tls, "354").map_err(|_| "DATA ready")?;
    send(
        &mut tls,
        "Subject: isolated M0 probe\r\n\r\nsynthetic content\r\n.\r\n",
    )
    .map_err(|_| "synthetic body")?;
    if expect(&mut tls, "250").is_err() {
        return Ok(Outcome::DeliveryUncertain);
    }
    Ok(Outcome::Accepted)
}

fn experiment(scenario: Scenario) -> Result<(Outcome, Observation), String> {
    let fixture = certificates()?;
    let directory = tempfile::tempdir().map_err(|_| "temporary SMTP directory")?;
    let marker = if matches!(scenario, Scenario::MarkerUnavailable) {
        directory.path().join("missing-parent").join("data-stage")
    } else {
        directory.path().join("data-stage")
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|_| "loopback bind")?;
    let address = listener.local_addr().map_err(|_| "loopback address")?;
    let server_marker = marker.clone();
    let handle = thread::spawn(move || server(listener, fixture.server, scenario, server_marker));
    let outcome = client(address, fixture.root, scenario, &marker);
    let mut observed = handle.join().map_err(|_| "fixture thread panic")??;
    observed.marker_after_result = fs::read(&marker)
        .map(|bytes| bytes == b"data_started\n")
        .unwrap_or(false);
    Ok((outcome?, observed))
}

pub fn smtp_tls_probe() -> Result<(), String> {
    let (outcome, observed) = experiment(Scenario::Success)?;
    if !matches!(outcome, Outcome::Accepted) || !observed.saw_data {
        return Err("trusted test CA failed".into());
    }
    let (outcome, observed) = experiment(Scenario::Untrusted)?;
    if !matches!(outcome, Outcome::TlsRejected) || observed.saw_data {
        return Err("untrusted test CA did not fail closed".into());
    }
    let (outcome, observed) = experiment(Scenario::NoStartTls)?;
    if !matches!(outcome, Outcome::DowngradeRejected) || observed.saw_data {
        return Err("STARTTLS downgrade did not fail closed".into());
    }
    super::record(
        "smtp_local_tls_probe",
        "loopback SMTP STARTTLS / rustls",
        "rustls=0.23.46; rcgen=0.14.5",
        &["std", "ring", "tls12"],
        "trusted ephemeral CA accepted; untrusted CA rejected before envelope; missing STARTTLS rejected; no external mail",
    )
}

pub fn smtp_stage_probe() -> Result<(), String> {
    let (outcome, observed) = experiment(Scenario::Success)?;
    if !matches!(outcome, Outcome::Accepted)
        || !observed.durable_before_data
        || !observed.marker_after_result
    {
        return Err("durable DATA ordering not observed".into());
    }
    let (outcome, observed) = experiment(Scenario::MarkerUnavailable)?;
    if !matches!(outcome, Outcome::MarkerRejected) || observed.saw_data {
        return Err("DATA sent despite persistence failure".into());
    }
    let (outcome, observed) = experiment(Scenario::DataDisconnect)?;
    if !matches!(outcome, Outcome::DeliveryUncertain)
        || !observed.durable_before_data
        || !observed.marker_after_result
    {
        return Err("post-DATA disconnect did not preserve uncertain classification".into());
    }
    super::record(
        "smtp_data_stage_observable",
        "auditable SMTP stage experiment + synced file",
        "rustls=0.23.46",
        &["sync_all before DATA"],
        "server read durable data_started before DATA acknowledgement; failed persistence prevented DATA; post-body disconnect classified uncertain; no retry loop",
    )
}
