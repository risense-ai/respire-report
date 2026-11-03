use agentnotify_core::{clock::Clock, contracts::*, delivery::oauth::*};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Default)]
struct Store(Mutex<HashMap<uuid::Uuid, (Vec<u8>, [u8; 32])>>);
impl CredentialStore for Store {
    fn get(&self, r: &CredentialRef) -> Result<CredentialHandle, CredentialError> {
        self.0
            .lock()
            .unwrap()
            .get(&r.id)
            .cloned()
            .map(|(bytes, revision)| {
                CredentialHandle::from_versioned_secret(
                    SecretInput::new(bytes),
                    CredentialRevision::from_bytes(revision),
                )
            })
            .ok_or(CredentialError {
                code: CredentialErrorCode::Missing,
            })
    }
    fn put(&self, r: &CredentialRef, s: SecretInput) -> Result<(), CredentialError> {
        let revision = uuid::Uuid::new_v4()
            .as_bytes()
            .repeat(2)
            .try_into()
            .unwrap();
        self.0
            .lock()
            .unwrap()
            .insert(r.id, (s.with_secret(|v| v.to_vec()), revision));
        Ok(())
    }
    fn delete(&self, r: &CredentialRef) -> Result<(), CredentialError> {
        self.0.lock().unwrap().remove(&r.id);
        Ok(())
    }
    // 仅同步既有fixture到版本CAS契约；比较与替换处于同一Mutex临界区，不新增用例。
    fn replace_if_revision(
        &self,
        r: &CredentialRef,
        expected: &CredentialRevision,
        input: SecretInput,
    ) -> Result<bool, CredentialError> {
        let mut entries = self.0.lock().unwrap();
        let Some((bytes, revision)) = entries.get_mut(&r.id) else {
            return Ok(false);
        };
        if &CredentialRevision::from_bytes(*revision) != expected {
            return Ok(false);
        }
        *bytes = input.with_secret(|v| v.to_vec());
        *revision = uuid::Uuid::new_v4()
            .as_bytes()
            .repeat(2)
            .try_into()
            .unwrap();
        Ok(true)
    }
}
struct TestClock(Mutex<u64>);
impl Clock for TestClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(100000 + *self.0.lock().unwrap() as i64, 0).unwrap()
    }
    fn monotonic(&self) -> Duration {
        Duration::from_secs(*self.0.lock().unwrap())
    }
}
struct Endpoint {
    challenge: Mutex<String>,
    refresh_failure: bool,
}
impl TokenEndpoint for Endpoint {
    fn exchange(
        &self,
        _: &OAuthProvider,
        _: &SecretInput,
        v: &SecretInput,
        _: &str,
    ) -> Result<TokenSet, OAuthError> {
        if v.with_secret(|b| URL_SAFE_NO_PAD.encode(Sha256::digest(b)))
            != *self.challenge.lock().unwrap()
        {
            return Err(OAuthError::ExchangeFailed);
        }
        Ok(TokenSet {
            access_token: SecretInput::new(b"isolated-access".to_vec()),
            refresh_token: Some(SecretInput::new(b"isolated-refresh".to_vec())),
            expires_in: 120,
        })
    }
    fn refresh(&self, _: &OAuthProvider, _: &SecretInput) -> Result<TokenSet, OAuthError> {
        if self.refresh_failure {
            return Err(OAuthError::RefreshFailed);
        }
        Ok(TokenSet {
            access_token: SecretInput::new(b"isolated-refreshed".to_vec()),
            refresh_token: None,
            expires_in: 3600,
        })
    }
    fn revoke(&self, _: &OAuthProvider, _: &SecretInput) -> Result<(), OAuthError> {
        Err(OAuthError::Unavailable)
    }
}
fn provider() -> OAuthProvider {
    OAuthProvider {
        client_id: "isolated-not-production".into(),
        authorization_url: "https://provider.invalid/authorize".into(),
        token_url: "https://provider.invalid/token".into(),
        revocation_url: None,
        scopes: vec!["smtp".into()],
    }
}
fn callback(r: &AuthorizationRequest, e: &Endpoint) -> OAuthCallback {
    let u = url::Url::parse(&r.url).unwrap();
    let q: HashMap<_, _> = u.query_pairs().into_owned().collect();
    assert_eq!(q["code_challenge_method"], "S256");
    assert!(!q.contains_key("nonce"));
    *e.challenge.lock().unwrap() = q["code_challenge"].clone();
    OAuthCallback {
        session_id: r.session_id,
        redirect_uri: q["redirect_uri"].clone(),
        state: q["state"].clone(),
        code: Some(SecretInput::new(b"isolated-code".to_vec())),
        error: None,
    }
}
#[test]
fn outlook_unavailable_without_publisher_registration() {
    assert!(
        !ProviderCatalog::bundled()
            .unwrap()
            .oauth_available("outlook")
    )
}
#[test]
fn unsigned_updates_unavailable() {
    assert!(matches!(
        ProviderCatalog::verify_update(b"{}", &[0; 64], None, 1),
        Err(OAuthError::Unavailable)
    ))
}
#[test]
fn state_transaction_expiry_denial_pkce_and_replay() {
    for kind in [
        "state",
        "transaction",
        "redirect",
        "expiry",
        "denied",
        "pkce",
        "ok",
    ] {
        let store = Store::default();
        let clock = Arc::new(TestClock(Mutex::new(0)));
        let ep = Endpoint {
            challenge: Mutex::new(String::new()),
            refresh_failure: false,
        };
        let mut s = OAuthSession::new(provider(), clock.clone(), &store, &ep).unwrap();
        let r = s.start().unwrap();
        let mut c = callback(&r, &ep);
        match kind {
            "state" => c.state.push('x'),
            "transaction" => c.session_id = uuid::Uuid::new_v4(),
            "redirect" => c.redirect_uri.push('x'),
            "expiry" => *clock.0.lock().unwrap() = 301,
            "denied" => c.error = Some("access_denied".into()),
            "pkce" => *ep.challenge.lock().unwrap() = "wrong".into(),
            _ => {}
        }
        let result = s.finish(c);
        assert_eq!(result.is_ok(), kind == "ok", "{kind}");
        assert!(s.finish(callback(&r, &ep)).is_err());
        assert_eq!(store.0.lock().unwrap().len(), usize::from(kind == "ok"));
    }
}
#[test]
fn refresh_failure_blocks_and_revoke_removes_local_tokens() {
    let store = Store::default();
    let clock = Arc::new(TestClock(Mutex::new(0)));
    let ep = Endpoint {
        challenge: Mutex::new(String::new()),
        refresh_failure: true,
    };
    let mut s = OAuthSession::new(provider(), clock, &store, &ep).unwrap();
    let r = s.start().unwrap();
    let reference = s.finish(callback(&r, &ep)).unwrap();
    assert!(access_token(&store, &reference, &ep, 100000).is_ok());
    assert!(matches!(
        access_token(&store, &reference, &ep, 100120),
        Err(OAuthError::RefreshFailed)
    ));
    assert!(revoke(&store, &reference, &ep).is_err());
    assert!(store.get(&reference).is_err())
}
#[test]
fn signed_updates_tamper_and_rollback() {
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let bytes = br#"{"version":2,"providers":[]}"#;
    let sig = key.sign(bytes);
    assert!(
        ProviderCatalog::verify_update(bytes, sig.as_ref(), Some(key.public_key().as_ref()), 1)
            .is_ok()
    );
    assert!(
        ProviderCatalog::verify_update(bytes, sig.as_ref(), Some(key.public_key().as_ref()), 2)
            .is_err()
    );
    assert!(
        ProviderCatalog::verify_update(
            br#"{"version":3,"providers":[]}"#,
            sig.as_ref(),
            Some(key.public_key().as_ref()),
            1
        )
        .is_err()
    );
    assert!(ProviderCatalog::install_update(bytes, sig.as_ref(), 1).is_err());
}
#[test]
fn callback_loopback_listener() {
    use std::io::{Read, Write};
    let store = Store::default();
    let clock = Arc::new(TestClock(Mutex::new(0)));
    let ep = Endpoint {
        challenge: Mutex::new(String::new()),
        refresh_failure: false,
    };
    let mut s = OAuthSession::new(provider(), clock, &store, &ep).unwrap();
    let r = s.start().unwrap();
    let c = callback(&r, &ep);
    let u = url::Url::parse(&c.redirect_uri).unwrap();
    let address = format!("127.0.0.1:{}", u.port().unwrap());
    let mut socket = std::net::TcpStream::connect(&address).unwrap();
    write!(
        socket,
        "GET {}?state={}&code=isolated-code HTTP/1.1\r\nHost: {}\r\n\r\n",
        u.path(),
        c.state,
        address
    )
    .unwrap();
    let parsed = s.poll_callback().unwrap().unwrap();
    assert!(s.finish(parsed).is_ok());
    let mut reply = String::new();
    socket.read_to_string(&mut reply).unwrap();
    assert!(!reply.contains("isolated-code"));
    assert!(std::net::TcpStream::connect(address).is_err());
}

#[test]
fn https_exchange_refresh_revoke_real_loopback_tls() {
    use rustls::{
        ClientConfig, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
        pki_types::PrivatePkcs8KeyDer,
    };
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    let client = Arc::new(
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
    let worker = thread::spawn(move || {
        for n in 0..3 {
            let (tcp, _) = listener.accept().unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut tls = StreamOwned::new(ServerConnection::new(server.clone()).unwrap(), tcp);
            let mut bytes = vec![];
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                tls.read_exact(&mut b).unwrap();
                bytes.push(b[0]);
                assert!(bytes.len() < 8192);
            }
            let head = String::from_utf8(bytes).unwrap();
            let len: usize = head
                .lines()
                .find_map(|s| {
                    s.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|n| n.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; len];
            tls.read_exact(&mut body).unwrap();
            let params: HashMap<_, _> = url::form_urlencoded::parse(&body).into_owned().collect();
            assert!(!params.contains_key("client_secret"));
            let response = if n == 0 {
                assert_eq!(params["code_verifier"], "isolated-verifier");
                assert_eq!(params["grant_type"], "authorization_code");
                r#"{"access_token":"local-access","refresh_token":"local-refresh","expires_in":3600,"token_type":"Bearer"}"#
            } else if n == 1 {
                assert_eq!(params["grant_type"], "refresh_token");
                r#"{"access_token":"refreshed-access","expires_in":3600,"token_type":"Bearer"}"#
            } else {
                assert_eq!(params["token"], "local-refresh");
                "{}"
            };
            write!(tls,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
            tls.flush().unwrap();
        }
    });
    let endpoint = HttpsTokenEndpoint::with_tls(client);
    let mut p = provider();
    p.token_url = format!("https://127.0.0.1:{port}/token");
    p.revocation_url = Some(format!("https://127.0.0.1:{port}/revoke"));
    let t = endpoint
        .exchange(
            &p,
            &SecretInput::new(b"isolated-code".to_vec()),
            &SecretInput::new(b"isolated-verifier".to_vec()),
            "http://127.0.0.1:1234/callback",
        )
        .unwrap();
    assert!(t.access_token.with_secret(|s| s == b"local-access"));
    let t2 = endpoint
        .refresh(&p, t.refresh_token.as_ref().unwrap())
        .unwrap();
    assert!(t2.access_token.with_secret(|s| s == b"refreshed-access"));
    endpoint
        .revoke(&p, t.refresh_token.as_ref().unwrap())
        .unwrap();
    worker.join().unwrap();
}
