//! Black-box coverage for encrypted snapshot recovery and CLI-only key registration.
use ahrara::{
    auth::Identity,
    config::Config,
    core::{Core, Ownership},
};
use serde_json::Value;
use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn profile(path: &Path) -> (Config, std::path::PathBuf) {
    std::fs::create_dir_all(path).unwrap();
    let cfg = Config {
        database_path: path.join("inbox.db"),
        identity_path: path.join("identity.key"),
        api_token_path: path.join("api.token"),
        check_for_updates: false,
        ..Config::default()
    };
    let file = path.join("config.toml");
    std::fs::write(&file, toml::to_string(&cfg).unwrap()).unwrap();
    (cfg, file)
}

fn invoke(config: &Path, args: &[&str], input: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ahrara"))
        .arg("--config")
        .arg(config)
        .arg("--json")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test]
async fn encrypted_snapshot_restores_identity_addresses_and_email_without_overwriting() {
    let tmp = tempfile::tempdir().unwrap();
    let (cfg, source) = profile(&tmp.path().join("source"));
    let (core, _) = Core::open(cfg.clone()).unwrap();
    let recovery = core.identity.export();
    let address = core.create_address(Some("snapshot".into())).await.unwrap();
    let raw = b"From: test@example.test\r\nTo: recipient@example.test\r\nSubject: Private snapshot\r\n\r\nsynthetic-private-marker\r\n".to_vec();
    core.storage
        .insert(
            "cli-snapshot-test".into(),
            core.identity.fingerprint(),
            vec![address.address.clone()],
            "test@example.test".into(),
            raw.clone(),
        )
        .await
        .unwrap();
    drop(core);
    let snapshot = tmp.path().join("export.db");
    let args = ["database", "export", snapshot.to_str().unwrap()];
    assert_eq!(success(&invoke(&source, &args, None))["exported"], true);
    let original = std::fs::read(&snapshot).unwrap();
    assert!(!original.starts_with(b"SQLite format 3\0"));
    assert!(
        !original
            .windows(24)
            .any(|part| part == b"synthetic-private-marker")
    );
    assert!(!invoke(&source, &args, None).status.success());
    assert_eq!(std::fs::read(&snapshot).unwrap(), original);

    let (restored_cfg, restored) = profile(&tmp.path().join("restored"));
    let import_args = ["database", "import", snapshot.to_str().unwrap()];
    assert!(!invoke(&restored, &import_args, None).status.success());
    assert!(!restored_cfg.identity_path.exists());
    let registered = invoke(&restored, &["auth", "set", "--stdin"], Some(&recovery));
    assert_eq!(success(&registered)["registered"], true);
    assert!(!String::from_utf8_lossy(&registered.stdout).contains(recovery.as_str()));
    assert!(!String::from_utf8_lossy(&registered.stderr).contains(recovery.as_str()));
    assert_eq!(
        success(&invoke(&restored, &import_args, None))["imported"],
        true
    );
    assert!(!invoke(&restored, &import_args, None).status.success());
    let bytes = std::fs::read(&restored_cfg.identity_path).unwrap();
    assert!(
        !invoke(&restored, &["auth", "set", "--stdin"], Some(&recovery))
            .status
            .success()
    );
    assert_eq!(std::fs::read(&restored_cfg.identity_path).unwrap(), bytes);
    let list = success(&invoke(&restored, &["inbox", "list"], None));
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (core, created) = Core::open(restored_cfg).unwrap();
    assert!(!created);
    assert_eq!(core.storage.email(1).await.unwrap().1, raw);
    assert_eq!(
        core.storage
            .get_address(core.identity.fingerprint(), address.address)
            .await
            .unwrap()
            .label
            .as_deref(),
        Some("snapshot")
    );
}

#[test]
fn wrong_key_and_active_owner_preserve_existing_inbox() {
    let tmp = tempfile::tempdir().unwrap();
    let (cfg, file) = profile(&tmp.path().join("source"));
    let (core, _) = Core::open(cfg.clone()).unwrap();
    let recovery = core.identity.export();
    drop(core);
    let snapshot = tmp.path().join("export.db");
    let export = ["database", "export", snapshot.to_str().unwrap()];
    {
        let _owner = Ownership::acquire(&cfg, "test listener", "").unwrap();
        assert!(!invoke(&file, &export, None).status.success());
        assert!(!snapshot.exists());
    }
    success(&invoke(&file, &export, None));
    let (wrong_cfg, wrong) = profile(&tmp.path().join("wrong"));
    let (_, _) = Identity::load_or_create(&wrong_cfg).unwrap();
    assert!(
        !invoke(
            &wrong,
            &["database", "import", snapshot.to_str().unwrap()],
            None
        )
        .status
        .success()
    );
    assert!(!wrong_cfg.database_path.exists());
    let before = std::fs::read(&cfg.database_path).unwrap();
    for args in [
        vec!["auth", "import", "--stdin", "--yes"],
        vec!["auth", "regenerate", "--yes"],
    ] {
        assert!(!invoke(&file, &args, Some(&recovery)).status.success());
        assert_eq!(std::fs::read(&cfg.database_path).unwrap(), before);
    }
    std::fs::remove_file(&cfg.identity_path).unwrap();
    assert!(!invoke(&file, &["status"], None).status.success());
    assert!(!cfg.identity_path.exists());
    success(&invoke(&file, &["auth", "set", "--stdin"], Some(&recovery)));
    success(&invoke(&file, &["status"], None));
}
