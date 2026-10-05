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
fn default_relay_connects_at_the_domain_root() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "").unwrap();

    assert_eq!(
        Config::load(Some(&path)).unwrap().server_url,
        "wss://ahrara.dev"
    );
}

#[tokio::test]
async fn custom_domain_is_local_default_with_stable_internal_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let (core, _) = Core::open(cfg.clone()).unwrap();
    let fp = core.identity.fingerprint();
    let target = core.create_address(None).await.unwrap();
    let domain = core
        .storage
        .set_custom_domain(fp.clone(), "AB.com".into(), target.id.to_string())
        .await
        .unwrap();

    assert_eq!(domain.domain, "ab.com");
    assert_eq!(domain.forwarding_address, target.address);
    let first = core.create_address(None).await.unwrap();
    let second = core.create_address(None).await.unwrap();

    assert!(first.address.ends_with("@ab.com"));
    assert_ne!(first.address, second.address);
    assert_eq!(first.forwarding_address, target.address);
    assert_eq!(
        first.routing_address,
        first.address.replace("@ab.com", "@ahrara.dev")
    );
    assert_eq!(first.route_id, target.route_id);
    assert!(
        core.storage
            .accepts(fp.clone(), first.routing_address.clone())
            .await
            .unwrap()
    );
    assert!(
        !core
            .storage
            .accepts(fp.clone(), first.address.clone())
            .await
            .unwrap()
    );
    assert_eq!(
        core.storage
            .get_address(fp.clone(), first.address.clone())
            .await
            .unwrap()
            .id,
        first.id
    );
    assert_eq!(
        core.storage
            .get_address(fp.clone(), first.routing_address.clone())
            .await
            .unwrap()
            .id,
        first.id
    );
    let raw = format!(
        "To: signup@ab.com\r\nX-Forwarded-To: {}\r\n\r\nbody\r\n",
        target.address
    )
    .into_bytes();
    let saved = core
        .storage
        .insert(
            "forwarded".into(),
            fp.clone(),
            vec![target.address.clone()],
            "sender@example.test".into(),
            raw.clone(),
        )
        .await
        .unwrap();
    let parsed = core.get_email(saved[0].id, 100).await.unwrap();

    assert_eq!(
        parsed.forwarded_to.as_deref(),
        Some(target.address.as_str())
    );
    assert_eq!(parsed.metadata.to_header, "signup@ab.com");
    assert_eq!(core.storage.email(saved[0].id).await.unwrap().1, raw);
    assert_eq!(
        core.storage
            .list(
                Filters {
                    address: Some(first.address.clone()),
                    ..Default::default()
                },
                Some(fp.clone())
            )
            .await
            .unwrap()
            .len(),
        1
    );

    for invalid in [
        "ahrara.dev",
        "https://ab.com",
        "ab.com/path",
        "a@ab.com",
        "-ab.com",
        "ab..com",
    ] {
        assert!(
            core.storage
                .set_custom_domain(fp.clone(), invalid.into(), target.address.clone())
                .await
                .is_err()
        );
    }

    core.change_address(target.id.to_string(), Some(false))
        .await
        .unwrap();

    assert!(core.create_address(None).await.is_err());
    core.change_address(target.id.to_string(), Some(true))
        .await
        .unwrap();
    drop(core);
    let (reopened, _) = Core::open(cfg).unwrap();

    assert!(
        reopened
            .create_address(None)
            .await
            .unwrap()
            .address
            .ends_with("@ab.com")
    );
    reopened
        .storage
        .remove_custom_domain(fp.clone())
        .await
        .unwrap();

    assert!(
        reopened
            .create_address(None)
            .await
            .unwrap()
            .address
            .ends_with("@ahrara.dev")
    );

    assert_eq!(
        reopened
            .storage
            .get_address(fp, first.id.to_string())
            .await
            .unwrap()
            .address,
        first.address
    );
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
    let fixture: Value =
        serde_json::from_str(include_str!("../../../protocol/vectors.json")).unwrap();
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
    assert_eq!(core.storage.schema_version().await.unwrap(), 5);
    drop(core);

    assert_eq!(
        Storage::open(
            &dir.path().join("ahrara.db"),
            &Identity::load(&config(dir.path())).unwrap().database_key()
        )
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

#[tokio::test]
async fn mime_budget_failure_preserves_original_eml() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = Core::open(config(dir.path())).unwrap();
    let address = core.create_address(None).await.unwrap();
    let raw = format!("Subject: retained original\r\nContent-Disposition: attachment; filename={}\r\n\r\nbody\r\n", "x".repeat(16 * 1024)).into_bytes();
    let saved = core
        .storage
        .insert(
            "mime-budget".into(),
            core.identity.fingerprint(),
            vec![address.address],
            "sender@example.test".into(),
            raw.clone(),
        )
        .await
        .unwrap();
    let id = saved[0].id;
    let parsed = core.get_email(id, 1024).await.unwrap();

    assert!(parsed.parse_error);
    assert!(parsed.attachments.is_empty());
    assert_eq!(core.storage.email(id).await.unwrap().1, raw);
    assert!(ahrara::mail::attachment(&raw, "1").is_err());
}

#[tokio::test]
async fn indexed_queries_preserve_filters_identity_order_and_stats() {
    let dir = tempfile::tempdir().unwrap();
    let (core, _) = Core::open(config(dir.path())).unwrap();
    let a = core.create_address(None).await.unwrap();
    let b = core.create_address(None).await.unwrap();
    let fp = core.identity.fingerprint();

    for identity in [&fp, "other identity"] {
        for transfer in ["first", "second"] {
            core.storage.insert(format!("{identity}-{transfer}"), identity.into(),
                vec![a.address.clone(), b.address.clone()], "bounce@example.test".into(),
                b"From: Sender <from@example.test>\r\nTo: somebody@example.test\r\nSubject: SQL ' % _ ;\r\n\r\nbody\r\n".to_vec()).await.unwrap();
        }
    }

    // Exercise all combinations of optional filters, including an Identity
    // restriction used by wait, and literal punctuation in substring filters.
    for mask in 0..64 {
        let filters = Filters {
            address: (mask & 1 != 0).then(|| a.address.replace("@ahrara.dev", "@AHRARA.DEV")),
            from_contains: (mask & 2 != 0).then(|| "BOUNCE".into()),
            to_contains: (mask & 4 != 0).then(|| "SOMEBODY".into()),
            subject_contains: (mask & 8 != 0).then(|| "' % _ ;".into()),
            after_id: Some(1),
            limit: Some(100),
        };
        let identity = (mask & 16 != 0).then(|| fp.clone());
        let mut expected: Vec<_> = (2..=8)
            .filter(|id| (mask & 1 == 0 || id % 2 == 1) && (mask & 16 == 0 || *id <= 4))
            .collect();
        let mut filters = filters;
        if mask & 32 != 0 {
            filters.limit = Some(1);
            expected.truncate(1);
        }

        let actual = core.storage.list(filters, identity).await.unwrap();

        assert_eq!(
            actual.iter().map(|email| email.id).collect::<Vec<_>>(),
            expected
        );
    }
    if a.address != a.address.to_ascii_lowercase() {
        assert!(
            core.storage
                .list(
                    Filters {
                        address: Some(a.address.to_ascii_lowercase()),
                        ..Default::default()
                    },
                    None
                )
                .await
                .unwrap()
                .is_empty()
        );
    }

    for id in [a.id.to_string(), a.address.clone()] {
        assert_eq!(
            core.storage
                .get_address(fp.clone(), id.clone())
                .await
                .unwrap()
                .id,
            a.id
        );

        assert!(
            core.storage
                .get_address("other identity".into(), id)
                .await
                .is_err()
        );
    }

    // Keep canonical numeric ID handling: leading zero or +prefix is not an ID.
    for id in [
        format!("0{}", a.id),
        format!("+{}", a.id),
        "9223372036854775808".into(),
    ] {
        assert!(core.storage.get_address(fp.clone(), id).await.is_err());
    }

    core.change_address(a.address.clone(), Some(false))
        .await
        .unwrap();

    assert_eq!(
        core.storage.address_count(fp.clone(), true).await.unwrap(),
        1
    );
    assert_eq!(
        core.storage
            .addresses(fp.clone(), 0, 20, true)
            .await
            .unwrap()[0]
            .id,
        b.id
    );
    assert_eq!(
        core.storage
            .addresses(fp.clone(), 0, 20, false)
            .await
            .unwrap()
            .len(),
        2
    );
    let before = core.storage.stats().await.unwrap();

    assert_eq!(before.0, 8);
    assert_eq!(before.2, 8);
    assert!(before.1.is_some());
    assert_eq!(core.storage.delete(Some(8)).await.unwrap(), 1);
    assert_eq!(core.storage.delete(Some(8)).await.unwrap(), 0);
    let after = core.storage.stats().await.unwrap();

    assert_eq!((after.0, after.2), (7, 7));
    assert_eq!(core.storage.delete(None).await.unwrap(), 7);
    assert_eq!(core.storage.stats().await.unwrap(), (0, None, 0));
}

#[tokio::test]
async fn query_index_migration_preserves_existing_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v2.db");
    let (identity, _) = Identity::load_or_create(&config(dir.path())).unwrap();
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(include_str!("../../core/migrations/001_messages.sql"))
            .unwrap();
        c.execute_batch(include_str!("../../core/migrations/002_addresses.sql"))
            .unwrap();
        c.pragma_update(None, "user_version", 2).unwrap();
        c.execute(
            "INSERT INTO messages(transfer_id,identity_fingerprint,recipient,envelope_sender,received_at,from_header,to_header,subject,size_bytes,raw_eml) VALUES('existing','fp','recipient','sender','timestamp','','','',3,?1)",
            [b"\0\r\n".as_slice()]
        ).unwrap();
        c.execute(
            "INSERT INTO addresses(identity_fingerprint,route_id,alias_id,address,created_at) VALUES('fp',zeroblob(16),zeroblob(8),'existing','timestamp')",
            []
        ).unwrap();
    }

    for _ in 0..2 {
        let storage = Storage::open(&path, &identity.database_key()).unwrap();
        assert_eq!(storage.schema_version().await.unwrap(), 5);
        assert_eq!(
            storage.stats().await.unwrap(),
            (1, Some("timestamp".into()), 1)
        );
        assert_eq!(storage.email(1).await.unwrap().1, b"\0\r\n");
        assert_eq!(storage.address_count("fp".into(), true).await.unwrap(), 1);
        assert_eq!(
            storage
                .get_address("fp".into(), "1".into())
                .await
                .unwrap()
                .address,
            "existing"
        );
    }
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
    // Created by Ahrara; existing caller-owned directories keep their mode.
    let profile = dir.path().join("profile");
    let cfg = config(&profile);
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
            std::fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn simultaneous_first_database_open() {
    for _ in 0..30 {
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let (identity, _) = Identity::load_or_create(&config(dir.path())).unwrap();
        let key = identity.database_key();
        let mut threads = Vec::new();

        for _ in 0..3 {
            let barrier = barrier.clone();
            let path = dir.path().join("new.db");
            let key = key.clone();
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                Storage::open(&path, &key)
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
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi.yaml");

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
    let second_connection =
        Storage::open(&cfg.database_path, &core.identity.database_key()).unwrap();
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
    cfg.server_url = "wss://127.0.0.1:1".into();
    cfg.server_public_key =
        Some(ahrara::secure::keygen(&dir.path().join("server-noise.key")).unwrap());
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
