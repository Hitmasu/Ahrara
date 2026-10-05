use ahrara::{config::Config, core::Ownership};
use rmcp::{ServiceExt, model::*, service::RunningService, transport::async_rw::AsyncRwTransport};
use serde_json::json;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::{Child, Command};

async fn client(path: &Path) -> (Child, RunningService<rmcp::RoleClient, ()>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ahrara"))
        .arg("--config")
        .arg(path)
        .args(["mcp", "--transport", "stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let transport =
        AsyncRwTransport::new_client(child.stdout.take().unwrap(), child.stdin.take().unwrap());
    let service = tokio::time::timeout(Duration::from_secs(15), ().serve(transport))
        .await
        .unwrap()
        .unwrap();

    (child, service)
}

fn profile() -> (tempfile::TempDir, Config, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let bind = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let cfg = Config {
        server_url: "wss://localhost:1".into(),
        server_public_key: None,
        identity_path: dir.path().join("identity.key"),
        database_path: dir.path().join("inbox.db"),
        api_token_path: dir.path().join("api.token"),
        api_tls_cert_path: dir.path().join("api.pem"),
        api_tls_key_path: dir.path().join("api-key.pem"),
        api_bind: bind.to_string(),
        check_for_updates: false,
        ..Config::default()
    };
    let path = dir.path().join("config.toml");
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    (dir, cfg, path)
}

#[tokio::test]
async fn agents_share_one_receiver_and_closing_one_keeps_the_other_alive() {
    let (_dir, cfg, path) = profile();
    let ((mut first, a), (mut second, b)) = tokio::join!(client(&path), client(&path));
    let owner = Ownership::current_http(&cfg).unwrap().unwrap();
    let tools = a.list_all_tools().await.unwrap();

    assert_eq!(tools.len(), 17);
    let result = a
        .call_tool(CallToolRequestParam {
            name: "ahrara_create_address".into(),
            arguments: json!({"label": "Agent test"}).as_object().cloned(),
        })
        .await
        .unwrap();

    assert_ne!(result.is_error, Some(true));
    let address = result.structured_content.unwrap();
    let result = b
        .call_tool(CallToolRequestParam {
            name: "ahrara_get_address".into(),
            arguments: json!({"address_or_id": address["address"]})
                .as_object()
                .cloned(),
        })
        .await
        .unwrap();

    assert_eq!(result.structured_content.unwrap()["label"], "Agent test");
    a.cancel().await.unwrap();

    assert!(
        tokio::time::timeout(Duration::from_secs(5), first.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert_eq!(
        Ownership::current_http(&cfg).unwrap().unwrap().pid,
        owner.pid
    );
    assert!(!b.list_all_resources().await.unwrap().is_empty());
    // Reception can be stopped while the shared MCP server remains attachable.
    b.call_tool(CallToolRequestParam {
        name: "ahrara_stop".into(),
        arguments: None,
    })
    .await
    .unwrap();
    let (mut third, c) = client(&path).await;

    assert_eq!(
        Ownership::current_http(&cfg).unwrap().unwrap().pid,
        owner.pid
    );
    c.cancel().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), third.wait())
        .await
        .unwrap()
        .unwrap();
    b.cancel().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), second.wait())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while Ownership::current_http(&cfg).unwrap().is_some() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();

    assert!(cfg.identity_path.exists() && cfg.database_path.exists());
    // MCP requires a bearer token by default; the bridge uses the created token.
    assert!(cfg.api_token_path.exists());
}

#[tokio::test]
async fn agent_reuses_a_manual_server_without_stopping_it() {
    for secure in [false, true] {
        let (_dir, cfg, path) = profile();
        let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        std::fs::write(&cfg.api_tls_cert_path, cert.cert.pem()).unwrap();
        ahrara::config::private_write(
            &cfg.api_tls_key_path,
            cert.signing_key.serialize_pem().as_bytes(),
            false,
        )
        .unwrap();
        let mut manual = Command::new(env!("CARGO_BIN_EXE_ahrara"))
            .arg("--config")
            .arg(&path)
            .arg("mcp")
            .args(if secure {
                vec!["--https", "--mcp-auth"]
            } else {
                vec![]
            })
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while tokio::net::TcpStream::connect(&cfg.api_bind).await.is_err() {
                assert!(manual.try_wait().unwrap().is_none());
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        let (mut child, agent) = client(&path).await;

        assert_eq!(
            Ownership::current_http(&cfg).unwrap().unwrap().pid,
            manual.id().unwrap()
        );
        agent.cancel().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1200)).await;

        assert!(manual.try_wait().unwrap().is_none());
        manual.kill().await.unwrap();
    }
}
