use agentnotify_core::{contracts::*, queue::BodyProtector};
use agentnotify_platform::credentials::NativeCredentialStore;
/// Cleanup owns only references created inside a fresh random test namespace.
struct Cleanup<'a> {
    store: &'a NativeCredentialStore,
    refs: Vec<CredentialRef>,
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        for r in &self.refs {
            let _ = self.store.delete(r);
        }
    }
}
#[test]
#[ignore = "writes only a random isolated native namespace"]
fn native_random_namespace_roundtrip_and_content_key() {
    let store = NativeCredentialStore::isolated(uuid::Uuid::new_v4());
    let mut cleanup = Cleanup {
        store: &store,
        refs: vec![],
    };
    let r = CredentialRef {
        id: uuid::Uuid::new_v4(),
    };
    cleanup.refs.push(r.clone());
    store
        .put(&r, SecretInput::new(b"isolated-native-test-only".to_vec()))
        .unwrap();
    let secret = store.get(&r).unwrap();
    assert!(secret.with_secret(|s| s == b"isolated-native-test-only"));
    store.delete(&r).unwrap();
    assert!(store.get(&r).is_err());
    let (key, p) = store.create_content_key().unwrap();
    cleanup.refs.push(key.clone());
    let sealed = p.seal(b"isolated body", b"metadata").unwrap();
    let second = store.load_content_key(&key).unwrap();
    assert!(second.open(&sealed, b"metadata").unwrap() == b"isolated body");
    assert!(second.open(&sealed, b"tamper").is_err());
    store.delete(&key).unwrap();
    assert!(store.load_content_key(&key).is_err());
}
#[test]
#[ignore = "parallel initialization uses only random isolated native namespace"]
fn simultaneous_content_key_initializers_never_overwrite() {
    let store = NativeCredentialStore::isolated(uuid::Uuid::new_v4());
    let pairs = std::thread::scope(|scope| {
        let a = scope.spawn(|| store.create_content_key().unwrap());
        let b = scope.spawn(|| store.create_content_key().unwrap());
        vec![a.join().unwrap(), b.join().unwrap()]
    });
    let cleanup = Cleanup {
        store: &store,
        refs: pairs.iter().map(|p| p.0.clone()).collect(),
    };
    assert_ne!(pairs[0].0.id, pairs[1].0.id);
    for (r, p) in pairs {
        let ciphertext = p.seal(b"independent fixture", b"aad").unwrap();
        assert!(
            store
                .load_content_key(&r)
                .unwrap()
                .open(&ciphertext, b"aad")
                .unwrap()
                == b"independent fixture"
        );
    }
    drop(cleanup);
}

#[test]
#[ignore = "large OAuth-shaped secret only in fresh random native namespace"]
fn native_large_oauth_secret_roundtrip() {
    let store = NativeCredentialStore::isolated(uuid::Uuid::new_v4());
    let r = CredentialRef {
        id: uuid::Uuid::new_v4(),
    };
    let _cleanup = Cleanup {
        store: &store,
        refs: vec![r.clone()],
    };
    let secret = uuid::Uuid::new_v4().as_bytes().repeat(512);
    store.put(&r, SecretInput::new(secret.clone())).unwrap();
    assert!(store.get(&r).unwrap().with_secret(|s| s == secret));
    store
        .put(&r, SecretInput::new(b"replacement".to_vec()))
        .unwrap();
    assert!(store.get(&r).unwrap().with_secret(|s| s == b"replacement"));
    store.delete(&r).unwrap();
    assert!(store.get(&r).is_err());
}

#[test]
#[ignore = "concurrent readers/writers use one random test credential"]
fn native_concurrent_read_replace() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = NativeCredentialStore::isolated(uuid::Uuid::new_v4());
    let r = CredentialRef {
        id: uuid::Uuid::new_v4(),
    };
    let _cleanup = Cleanup {
        store: &store,
        refs: vec![r.clone()],
    };
    store.put(&r, SecretInput::new(vec![7; 8192])).unwrap();
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut count = 0;
            while !done.load(Ordering::SeqCst) && count < 1000 {
                let handle = store.get(&r).unwrap();
                assert!(
                    handle.with_secret(|b| b.len() == 8192 && b.iter().all(|v| *v == 7 || *v == 8))
                );
                count += 1;
            }
            count
        });
        for n in 0..16 {
            store
                .put(
                    &r,
                    SecretInput::new(vec![if n % 2 == 0 { 8 } else { 7 }; 8192]),
                )
                .unwrap();
        }
        done.store(true, Ordering::SeqCst);
        assert!(reader.join().unwrap() > 0);
    });
}

#[test]
#[ignore = "interruption simulation touches only native entries in a random test namespace"]
fn native_pending_recovery_before_and_after_publication() {
    let ns = uuid::Uuid::new_v4();
    let store = NativeCredentialStore::isolated(ns);
    let r = CredentialRef {
        id: uuid::Uuid::new_v4(),
    };
    let _cleanup = Cleanup {
        store: &store,
        refs: vec![r.clone()],
    };
    store
        .put(&r, SecretInput::new(b"old fixture".to_vec()))
        .unwrap();
    let root = keyring::Entry::new(&format!("AgentNotify.Test.{ns}"), &r.id.to_string()).unwrap();
    let pending =
        keyring::Entry::new(&format!("AgentNotify.Test.{ns}.pending"), &r.id.to_string()).unwrap();
    for publish in [false, true] {
        let old = root.get_secret().unwrap();
        let part_id = uuid::Uuid::new_v4();
        let part =
            keyring::Entry::new(&format!("AgentNotify.Test.{ns}.part"), &part_id.to_string())
                .unwrap();
        let mut next = b"ANCRV001".to_vec();
        next.extend(11u32.to_le_bytes());
        next.extend(ring::digest::digest(&ring::digest::SHA256, b"new fixture").as_ref());
        next.extend(1u16.to_le_bytes());
        next.extend(part_id.as_bytes());
        let mut intent = b"ANCTX001".to_vec();
        intent.extend((next.len() as u16).to_le_bytes());
        intent.extend((old.len() as u16).to_le_bytes());
        intent.extend(&next);
        intent.extend(&old);
        pending.set_secret(&intent).unwrap();
        part.set_secret(b"new fixture").unwrap();
        if publish {
            root.set_secret(&next).unwrap();
        }
        let loaded = store.get(&r).unwrap();
        assert!(loaded.with_secret(|b| b
            == if publish {
                b"new fixture"
            } else {
                b"old fixture"
            }));
        assert!(matches!(pending.get_secret(), Err(keyring::Error::NoEntry)));
        if !publish {
            assert!(matches!(part.get_secret(), Err(keyring::Error::NoEntry)))
        } else {
            for id in old[46..].chunks_exact(16) {
                let old_part = keyring::Entry::new(
                    &format!("AgentNotify.Test.{ns}.part"),
                    &uuid::Uuid::from_bytes(id.try_into().unwrap()).to_string(),
                )
                .unwrap();
                assert!(matches!(
                    old_part.get_secret(),
                    Err(keyring::Error::NoEntry)
                ));
            }
        }
    }
    assert!(store.put(&r, SecretInput::new(vec![1; 65537])).is_err());
    assert!(store.get(&r).unwrap().with_secret(|b| b == b"new fixture"));
}
#[test]
#[ignore = "helper invoked only by isolated parent process test"]
fn native_process_reader() {
    let Ok(ns) = std::env::var("AN_TEST_CREDENTIAL_NAMESPACE") else {
        return;
    };
    let id = std::env::var("AN_TEST_CREDENTIAL_REF").unwrap();
    let store = NativeCredentialStore::isolated(uuid::Uuid::parse_str(&ns).unwrap());
    let r = CredentialRef {
        id: uuid::Uuid::parse_str(&id).unwrap(),
    };
    for _ in 0..64 {
        let h = store.get(&r).unwrap();
        assert!(h.with_secret(
            |b| b.len() == 8192 && (b.iter().all(|v| *v == 7) || b.iter().all(|v| *v == 8))
        ));
    }
}
#[test]
#[ignore = "two processes share only one fresh random native test namespace"]
fn native_cross_process_read_replace() {
    let ns = uuid::Uuid::new_v4();
    let store = NativeCredentialStore::isolated(ns);
    let r = CredentialRef {
        id: uuid::Uuid::new_v4(),
    };
    let _cleanup = Cleanup {
        store: &store,
        refs: vec![r.clone()],
    };
    store.put(&r, SecretInput::new(vec![7; 8192])).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_process_reader",
            "--ignored",
            "--test-threads=1",
        ])
        .env("AN_TEST_CREDENTIAL_NAMESPACE", ns.to_string())
        .env("AN_TEST_CREDENTIAL_REF", r.id.to_string())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for n in 0..16 {
        store
            .put(
                &r,
                SecretInput::new(vec![if n % 2 == 0 { 8 } else { 7 }; 8192]),
            )
            .unwrap();
    }
    assert!(child.wait().unwrap().success());
}
