use ahrara::{
    auth::{self, Identity},
    config::Config,
    core::{Core, Ownership},
    storage::{Filters, Storage},
};
use serde_json::Value;
use std::path::Path;

fn config(dir: &Path) -> Config {
    Config {
        database_path: dir.join("ahrara.db"),
        identity_path: dir.join("identity.key"),
        api_token_path: dir.join("api.token"),
        ..Config::default()
    }
}

#[test]
fn identity_recovery_and_shared_go_rust_vectors() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (one, _) = Identity::load_or_create(&config(a.path())).unwrap();
    Identity::import(&one.export(), &config(b.path())).unwrap();
    let two = Identity::load(&config(b.path())).unwrap();
    assert!(one.public_key() == two.public_key());
    assert!(one.route() == two.route());
    let fixture: Value = serde_json::from_str(include_str!("../../protocol/vectors.json")).unwrap();
    let public: [u8; 32] = hex::decode(fixture["public_key"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let route = auth::derive_route(&public);
    assert_eq!(hex::encode(route), fixture["route_id"]);
    let alias: [u8; 8] = hex::decode(fixture["alias_id"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let address = auth::address(&route, &alias);
    assert_eq!(address, fixture["address"]);
    assert_eq!(address.split('@').next().unwrap().len(), 32);
    assert_eq!(auth::decode_address(&address).unwrap().0, route);
    assert_eq!(
        auth::decode_address(&address.replace("@ahrara.dev", "@AHRARA.DEV"))
            .unwrap()
            .2,
        address
    );
    assert_ne!(address.to_ascii_lowercase(), address);
    assert_ne!(
        auth::decode_address(&address.to_ascii_lowercase())
            .unwrap()
            .0,
        route
    );
    assert!(Identity::import("not-a-recovery-key", &config(b.path())).is_err());
    assert!(one.sign(&[0; 31]).is_err());
}

#[tokio::test]
async fn aliases_collisions_disabled_and_history_survive_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = Core::open(config(dir.path())).unwrap();
    let a = core
        .storage
        .create_address_with(
            core.identity.fingerprint(),
            core.identity.route(),
            None,
            || [1; 8],
        )
        .await
        .unwrap();
    let mut attempt = 0;
    let b = core
        .storage
        .create_address_with(
            core.identity.fingerprint(),
            core.identity.route(),
            Some("second".into()),
            move || {
                attempt += 1;
                [attempt; 8]
            },
        )
        .await
        .unwrap();
    let c = core.create_address(None).await.unwrap();
    assert_ne!(a.alias_id, b.alias_id);
    assert_ne!(a.address, c.address);
    assert_eq!(a.route_id, b.route_id);
    assert!(
        core.storage
            .accepts(core.identity.fingerprint(), a.address.clone())
            .await
            .unwrap()
    );
    core.change_address(a.id.to_string(), Some(false))
        .await
        .unwrap();
    assert!(
        !core
            .storage
            .accepts(core.identity.fingerprint(), a.address.clone())
            .await
            .unwrap()
    );
    core.change_address(a.id.to_string(), Some(true))
        .await
        .unwrap();
    let raw=b"From: Sender <sender@example.test>\r\nTo: somebody@example.test\r\nSubject: OTP\r\n\r\n.line\r\n\xff\r\n".to_vec();
    let saved = core
        .storage
        .insert(
            "transfer".into(),
            core.identity.fingerprint(),
            vec![a.address.clone(), b.address.clone()],
            "sender@example.test".into(),
            raw.clone(),
        )
        .await
        .unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(core.storage.email(saved[0].id).await.unwrap().1, raw);
    core.change_address(a.id.to_string(), None).await.unwrap();
    assert_eq!(core.storage.stats().await.unwrap().0, 2);
    let matching = core
        .storage
        .list(
            Filters {
                address: Some(a.address),
                subject_contains: Some("otp".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(matching.len(), 1);
    let page = core.read_eml(saved[0].id, raw.len() - 3, 3).await.unwrap();
    assert_eq!(page["encoding"], "base64");
    assert!(core.read_eml(saved[0].id, 0, 256 * 1024 + 1).await.is_err());
    assert_eq!(core.storage.delete(Some(saved[0].id)).await.unwrap(), 1);
    assert_eq!(core.storage.delete(None).await.unwrap(), 1);
    assert_eq!(core.storage.schema_version().await.unwrap(), 2);
    drop(core);
    assert_eq!(
        Storage::open(&dir.path().join("ahrara.db"))
            .unwrap()
            .stats()
            .await
            .unwrap()
            .0,
        0
    );
}

#[tokio::test]
async fn failed_multi_recipient_commit_is_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = Core::open(config(dir.path())).unwrap();
    core.storage.call(|c|{c.execute_batch("CREATE TRIGGER stop_bad BEFORE INSERT ON messages WHEN NEW.recipient='bad' BEGIN SELECT RAISE(ABORT,'test failure'); END;")?;Ok(())}).await.unwrap();
    assert!(
        core.storage
            .insert(
                "transaction".into(),
                core.identity.fingerprint(),
                vec!["good".into(), "bad".into()],
                "sender".into(),
                b"Subject: rollback\r\n\r\nbody\r\n".to_vec()
            )
            .await
            .is_err()
    );
    assert_eq!(core.storage.stats().await.unwrap().0, 0);
}

#[test]
fn ownership_identifies_process_and_releases_after_drop() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let guard = Ownership::acquire(&cfg, "api", "test fingerprint").unwrap();
    let error = Ownership::acquire(&cfg, "listen", "test fingerprint")
        .err()
        .unwrap();
    assert!(error.to_string().contains("ahrara api"));
    assert_eq!(Ownership::current(&cfg).unwrap().unwrap().mode, "api");
    drop(guard);
    assert!(Ownership::current(&cfg).unwrap().is_none());
    assert!(Ownership::acquire(&cfg, "mcp", "test fingerprint").is_ok());
}

#[test]
fn token_is_independent_restricted_and_rotatable() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let (identity, _) = Identity::load_or_create(&cfg).unwrap();
    auth::generate_token(&cfg, false).unwrap();
    let before = std::fs::read_to_string(&cfg.api_token_path).unwrap();
    assert!(auth::validate_api_token(&before).is_ok());
    assert!(auth::validate_api_token("").is_err());
    assert!(auth::validate_api_token("short").is_err());
    assert!(!identity.export().contains(&before));
    auth::generate_token(&cfg, true).unwrap();
    let after = std::fs::read_to_string(&cfg.api_token_path).unwrap();
    assert!(before != after);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in [&cfg.api_token_path, &cfg.identity_path] {
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn simultaneous_first_database_open() {
    for _ in 0..30 {
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..3 {
            let barrier = barrier.clone();
            let path = dir.path().join("new.db");
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                Storage::open(&path)
            }));
        }
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        for result in &results {
            assert!(
                result.is_ok(),
                "simultaneous startup failed: {:?}",
                result.as_ref().err()
            );
        }
    }
}

#[test]
fn attachment_safety_and_exact_bytes() {
    let raw = include_bytes!("fixtures/message.eml");
    let (metadata, bytes) = ahrara::mail::attachment(raw, "1").unwrap();
    assert_eq!(bytes, b"hello attachment\n");
    assert_eq!(metadata.filename, "attachment.txt");
    let dir = tempfile::tempdir().unwrap();
    let saved = ahrara::mail::save_attachment(&metadata, &bytes, dir.path()).unwrap();
    assert_eq!(std::fs::read(&saved).unwrap(), bytes);
    assert!(ahrara::mail::save_attachment(&metadata, &bytes, dir.path()).is_err());
    for input in ["../../authorized_keys", "..\\..\\authorized_keys"] {
        assert_eq!(ahrara::mail::safe_filename(input), "authorized_keys");
    }
    assert_eq!(ahrara::mail::safe_filename("CON.txt"), "_CON.txt");
    assert!(!ahrara::mail::terminal_safe("\x1b]52;clipboard\x07").contains('\x1b'));
}

#[test]
fn openapi_contract() {
    let generated = ahrara::api::openapi_yaml().unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../openapi.yaml");
    assert_eq!(
        generated,
        std::fs::read_to_string(path).expect("run make openapi")
    );
    assert!(generated.contains("/v1/emails/wait:"));
    assert!(generated.contains("/v1/events:"));
    assert!(generated.contains("localToken:"));
    assert!(!generated.contains("Recovery Key"));
}

#[tokio::test]
async fn concurrent_alias_creation_and_recovery_without_sqlite() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let cfg = config(first.path());
    let (core, _) = Core::open(cfg.clone()).unwrap();
    let second_connection = Storage::open(&cfg.database_path).unwrap();
    let mut first_attempt = 0;
    let a = core.storage.create_address_with(
        core.identity.fingerprint(),
        core.identity.route(),
        None,
        move || {
            first_attempt += 1;
            [first_attempt; 8]
        },
    );
    let mut attempt = 0;
    let b = second_connection.create_address_with(
        core.identity.fingerprint(),
        core.identity.route(),
        None,
        move || {
            attempt += 1;
            [attempt; 8]
        },
    );
    let (a, b) = tokio::join!(a, b);
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.alias_id, b.alias_id);
    auth::Identity::import(&core.identity.export(), &config(second.path())).unwrap();
    let (restored, _) = Core::open(config(second.path())).unwrap();
    assert_eq!(restored.identity.route(), core.identity.route());
    assert_eq!(
        restored
            .storage
            .address_count(restored.identity.fingerprint(), false)
            .await
            .unwrap(),
        0
    );
    assert!(
        !restored
            .storage
            .accepts(restored.identity.fingerprint(), a.address)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn start_preserves_disabled_aliases_and_offline_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.server_url = "wss://127.0.0.1:1/ws".into();
    let (core, _) = Core::open(cfg.clone()).unwrap();
    let alias = core.create_address(None).await.unwrap();
    core.change_address(alias.id.to_string(), Some(false))
        .await
        .unwrap();
    core.start("api", false).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        core.storage
            .address_count(core.identity.fingerprint(), false)
            .await
            .unwrap(),
        1
    );
    assert!(core.status().await.unwrap().addresses.is_empty());
    assert!(Ownership::acquire(&cfg, "listen", &core.identity.fingerprint()).is_err());
    assert!(
        core.storage
            .create_address(core.identity.fingerprint(), core.identity.route(), None)
            .await
            .is_ok()
    );
    core.stop().await;
    assert!(Ownership::current(&cfg).unwrap().is_none());
}
