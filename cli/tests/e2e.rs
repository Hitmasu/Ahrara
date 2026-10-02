//! Real Go SMTP/WSS server, real Rust processes, isolated profiles and real TLS.
//! Run through make e2e / mcp-e2e / api-e2e after docker compose up --wait.
use ahrara::{auth, config::Config};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct TestProfile {
    _dir: tempfile::TempDir,
    config: Config,
    path: PathBuf,
    root: PathBuf,
    eml: PathBuf,
    raw: Vec<u8>,
}
impl TestProfile {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_owned();
        let _dir = tempfile::tempdir().unwrap();
        let dir = _dir.path();
        let dev: Config = toml::from_str(
            &std::fs::read_to_string(root.join(".dev/profile/config.toml"))
                .expect("run make dev-certs"),
        )
        .unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let config = Config {
            server_url: dev.server_url,
            smtp_dev_address: dev.smtp_dev_address,
            database_path: dir.join("ahrara.db"),
            identity_path: dir.join("identity.key"),
            api_token_path: dir.join("api.token"),
            tls_ca_path: Some(root.join(".dev/certs/ca.pem")),
            api_tls_cert_path: root.join(".dev/certs/server.pem"),
            api_tls_key_path: root.join(".dev/certs/server-key.pem"),
            api_bind: format!("127.0.0.1:{port}"),
            ..Config::default()
        };
        let path = dir.join("config.toml");
        std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
        let raw = include_str!("fixtures/message.eml")
            .replace("\r\n", "\n")
            .replace('\n', "\r\n")
            .into_bytes();
        let eml = dir.join("original.eml");
        std::fs::write(&eml, &raw).unwrap();
        Self {
            _dir,
            config,
            path,
            root,
            eml,
            raw,
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let binary = std::env::var_os("AHRARA_TEST_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_ahrara").into());
        let mut c = Command::new(binary);
        c.arg("--config").arg(&self.path).arg("--json").args(args);
        c
    }
    fn run(&self, args: &[&str]) -> Value {
        let o = self.command(args).output().unwrap();
        assert!(
            o.status.success(),
            "command failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let text = String::from_utf8(o.stdout).unwrap();
        assert!(!text.contains("ahrara_sk_v1_"));
        serde_json::from_str(&text).unwrap()
    }
    fn spawn(&self, mode: &str) -> Process {
        Process(
            self.command(&[mode])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
    fn ready(&self) -> Value {
        let until = Instant::now() + Duration::from_secs(25);
        loop {
            let s = self.run(&["status"]);
            if s["connected"] == true {
                return s;
            }
            assert!(Instant::now() < until, "listener did not connect: {s}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    fn send(&self, to: &str, file: Option<&Path>, size: usize) -> std::process::Output {
        Command::new(self.root.join("dist/ahrara-dev"))
            .arg("send")
            .arg("--addr")
            .arg(&self.config.smtp_dev_address)
            .arg("--ca")
            .arg(self.config.tls_ca_path.as_ref().unwrap())
            .arg("--to")
            .arg(to)
            .arg("--file")
            .arg(file.unwrap_or(&self.eml))
            .arg("--size")
            .arg(size.to_string())
            .output()
            .unwrap()
    }
    fn deliver(&self, to: &str) {
        let o = self.send(to, None, 0);
        assert!(
            o.status.success(),
            "SMTP failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

#[test]
#[ignore = "requires Docker SMTP/WSS server"]
fn cli_end_to_end() {
    let p = TestProfile::new();
    let aliases: Vec<_> = (0..3).map(|_| p.run(&["address", "create"])).collect();
    for a in &aliases {
        assert_eq!(a["route_id"], aliases[0]["route_id"]);
        assert_eq!(
            a["address"]
                .as_str()
                .unwrap()
                .split('@')
                .next()
                .unwrap()
                .len(),
            32
        );
    }
    let listener = p.spawn("listen");
    let before = p.ready();
    assert_eq!(before["addresses"].as_array().unwrap().len(), 3);
    let recipients = aliases
        .iter()
        .map(|v| v["address"].as_str().unwrap())
        .collect::<Vec<_>>();
    // Same SMTP transaction, three aliases, repeated RCPT is deduplicated.
    p.deliver(&format!(
        "{},{},{},{}",
        recipients[0], recipients[1], recipients[2], recipients[0]
    ));
    let messages = p.run(&["inbox", "list"]);
    assert_eq!(messages.as_array().unwrap().len(), 3);
    let transfers = messages
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["transfer_id"].clone())
        .collect::<Vec<_>>();
    assert!(transfers.iter().all(|t| t == &transfers[0]));
    for recipient in &recipients {
        let list = p.run(&[
            "inbox",
            "list",
            "--address",
            recipient,
            "--subject",
            "verification",
            "--from",
            "sender",
        ]);
        assert_eq!(list.as_array().unwrap().len(), 1);
        let id = list[0]["id"].as_i64().unwrap().to_string();
        let content = p.run(&["inbox", "show", &id]);
        assert!(content["html_available"].as_bool().unwrap());
        let raw = p.command(&["inbox", "eml", &id]).output().unwrap();
        assert!(raw.status.success());
        assert_eq!(raw.stdout, p.raw);
        let output = p._dir.path().join(format!("attachment-{id}"));
        p.run(&[
            "inbox",
            "attachment",
            "save",
            &id,
            "1",
            output.to_str().unwrap(),
        ]);
        assert_eq!(std::fs::read(output).unwrap(), b"hello attachment\n");
    }
    let (route, _, _) = auth::decode_address(recipients[0]).unwrap();
    let fabricated = auth::address(&route, &[0; 8]);
    let rejected = p.send(
        &fabricated,
        Some(Path::new("/this-file-must-not-be-read-before-RCPT")),
        0,
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("550"));
    assert_eq!(p.run(&["inbox", "list"]).as_array().unwrap().len(), 3);
    p.run(&["address", "disable", recipients[0]]);
    assert!(!p.send(recipients[0], None, 0).status.success());
    p.run(&["address", "enable", recipients[0]]);
    let busy = p.command(&["mcp"]).output().unwrap();
    assert!(!busy.status.success());
    assert!(String::from_utf8_lossy(&busy.stderr).contains("listen"));
    // Exact 10 MiB including CRLF; plus one byte must fail without a local commit.
    let mut boundary = b"Subject: size boundary\r\n\r\n".to_vec();
    while boundary.len() + 1002 <= 10 * 1024 * 1024 {
        boundary.extend(std::iter::repeat_n(b'x', 998));
        boundary.extend_from_slice(b"\r\n");
    }
    boundary.resize(10 * 1024 * 1024 - 2, b'x');
    boundary.extend_from_slice(b"\r\n");
    let file = p._dir.path().join("boundary.eml");
    std::fs::write(&file, &boundary).unwrap();
    let sent = p.send(recipients[0], Some(&file), boundary.len());
    assert!(
        sent.status.success(),
        "exact limit: {}",
        String::from_utf8_lossy(&sent.stderr)
    );
    boundary.insert(0, b'x');
    std::fs::write(&file, &boundary).unwrap();
    assert!(!p.send(recipients[0], Some(&file), 0).status.success());
    assert!(
        !p.send(recipients[0], Some(&file), boundary.len())
            .status
            .success()
    );
    assert_eq!(p.run(&["status"])["emails_count"], 4);
    drop(listener);
    std::thread::sleep(Duration::from_millis(300));
    let _restarted = p.spawn("listen");
    assert_eq!(p.ready()["route_id"], before["route_id"]);
    p.run(&["address", "delete", recipients[0]]);
    assert_eq!(p.run(&["status"])["emails_count"], 4);
    p.run(&["inbox", "clear"]);
    assert_eq!(p.run(&["status"])["emails_count"], 0);
}

// Speak JSON-RPC over actual stdio, keeping stdout protocol purity under test.
struct McpClient {
    process: Process,
    out: BufReader<std::process::ChildStdout>,
    next: u64,
}
impl McpClient {
    fn new(p: &TestProfile) -> Self {
        let mut process = Process(
            p.command(&["mcp"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let out = BufReader::new(process.0.stdout.take().unwrap());
        let mut c = Self {
            process,
            out,
            next: 0,
        };
        c.request("initialize",json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"ahrara-tests","version":"1"}}));
        c.write(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        c
    }
    fn write(&mut self, v: Value) {
        let input = self.process.0.stdin.as_mut().unwrap();
        writeln!(input, "{v}").unwrap();
        input.flush().unwrap();
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        self.write(json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params}));
        loop {
            let mut line = String::new();
            assert!(
                self.out.read_line(&mut line).unwrap() > 0,
                "MCP closed stdout"
            );
            assert!(!line.contains("ahrara_sk_v1_"));
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == self.next {
                assert!(v.get("error").is_none(), "MCP protocol error: {v}");
                return v["result"].clone();
            }
        }
    }
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name":name,"arguments":args}));
        assert_ne!(r["isError"], true, "{r}");
        r["structuredContent"].clone()
    }
}

#[test]
#[ignore = "requires Docker SMTP/WSS server"]
fn mcp_end_to_end() {
    let p = TestProfile::new();
    let mut c = McpClient::new(&p);
    let tools = c.request("tools/list", json!({}));
    assert_eq!(tools["tools"].as_array().unwrap().len(), 17);
    let start = c.tool("ahrara_start", json!({}));
    assert_eq!(start["connected"], true);
    assert_eq!(start["addresses"].as_array().unwrap().len(), 1);
    let address = c.tool("ahrara_create_address", json!({"label":"MCP"}));
    let recipient = address["address"].as_str().unwrap();
    assert_eq!(
        c.tool("ahrara_start", json!({}))["route_id"],
        start["route_id"]
    );
    c.tool("ahrara_get_address", json!({"address_or_id":recipient}));
    c.tool("ahrara_disable_address", json!({"address_or_id":recipient}));
    c.tool("ahrara_enable_address", json!({"address_or_id":recipient}));
    assert_eq!(
        c.tool("ahrara_list_addresses", json!({"limit":1}))["addresses"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    p.deliver(recipient);
    let received = c.tool(
        "ahrara_wait_for_email",
        json!({"address":recipient,"timeout_seconds":5}),
    );
    assert_eq!(received["status"], "matched");
    let id = received["email"]["id"].as_i64().unwrap();
    assert_eq!(
        c.tool("ahrara_list_emails", json!({"address":recipient}))["emails"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    c.tool("ahrara_get_email", json!({"id":id}));
    let eml = c.tool("ahrara_read_eml", json!({"id":id}));
    assert_eq!(eml["encoding"], "utf8");
    assert_eq!(eml["content"].as_str().unwrap().as_bytes(), p.raw);
    assert_eq!(
        c.tool("ahrara_list_attachments", json!({"id":id}))["attachments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    c.tool(
        "ahrara_save_attachment",
        json!({"email_id":id,"attachment_id":"1","destination":p._dir.path().join("saved.txt")}),
    );
    c.request("resources/list", json!({}));
    c.request("resources/templates/list", json!({}));
    for uri in [
        "ahrara://status".to_owned(),
        format!("ahrara://emails/{id}"),
        format!("ahrara://emails/{id}/eml"),
    ] {
        c.request("resources/read", json!({"uri":uri}));
    }
    assert_eq!(
        c.tool(
            "ahrara_wait_for_email",
            json!({"address":recipient,"after_id":id,"timeout_seconds":1})
        )["status"],
        "timeout"
    );
    c.tool("ahrara_delete_email", json!({"id":id}));
    // A cancelled wait must not block subsequent requests or stop the listener.
    c.next += 1;
    let cancelled = c.next;
    c.write(json!({"jsonrpc":"2.0","id":cancelled,"method":"tools/call","params":{"name":"ahrara_wait_for_email","arguments":{"after_id":id,"timeout_seconds":300}}}));
    std::thread::sleep(Duration::from_millis(20));
    c.write(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":cancelled,"reason":"test cancellation"}}));
    assert_eq!(c.tool("ahrara_status", json!({}))["connected"], true);
    c.tool("ahrara_clear_inbox", json!({}));
    c.tool("ahrara_delete_address", json!({"address_or_id":recipient}));
    c.tool("ahrara_stop", json!({}));
    assert_eq!(c.tool("ahrara_status", json!({}))["connected"], false);
    let _listener = p.spawn("listen");
    assert_eq!(p.ready()["route_id"], start["route_id"]);
}

#[tokio::test]
#[ignore = "requires Docker SMTP/WSS server"]
async fn api_end_to_end() {
    let p = TestProfile::new();
    let _api = p.spawn("api");
    p.ready();
    let base = format!("https://{}", p.config.api_bind);
    let client = reqwest::Client::builder()
        .add_root_certificate(
            reqwest::Certificate::from_pem(
                &std::fs::read(p.config.tls_ca_path.as_ref().unwrap()).unwrap(),
            )
            .unwrap(),
        )
        .build()
        .unwrap();
    let token = std::fs::read_to_string(&p.config.api_token_path).unwrap();
    assert!(
        reqwest::Client::new()
            .get(format!("{base}/healthz"))
            .send()
            .await
            .is_err()
    );
    assert_eq!(
        client
            .get(format!("{base}/healthz"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    for route in ["/v1/status", "/v1/addresses", "/openapi.json", "/v1/events"] {
        assert_eq!(
            client
                .get(format!("{base}{route}"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client
                .get(format!("{base}{route}"))
                .bearer_auth("invalid")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    let status: Value = client
        .get(format!("{base}/v1/status"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["connected"], true);
    assert!(!status.to_string().contains("ahrara_sk_v1_"));
    assert!(!status.to_string().contains(&token));
    let spec: Value = client
        .get(format!("{base}/openapi.json"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(spec["openapi"], "3.1.0");
    let mut events = client
        .get(format!("{base}/v1/events"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .bytes_stream();
    let a: Value = client
        .post(format!("{base}/v1/addresses"))
        .bearer_auth(&token)
        .json(&json!({"label":"API"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let recipient = a["address"].as_str().unwrap().to_owned();
    let aid = a["id"].as_i64().unwrap();
    let mut observed = String::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !observed.contains("address_created") {
            observed.push_str(std::str::from_utf8(&events.next().await.unwrap().unwrap()).unwrap());
        }
    })
    .await
    .unwrap();
    for action in ["disable", "enable"] {
        assert_eq!(
            client
                .post(format!("{base}/v1/addresses/{aid}/{action}"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    assert_eq!(
        client
            .get(format!("{base}/v1/addresses/{aid}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let page: Value = client
        .get(format!("{base}/v1/addresses?limit=1"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page.as_array().unwrap().len(), 1);
    let waiter = tokio::spawn({
        let client = client.clone();
        let url = format!("{base}/v1/emails/wait");
        let token = token.clone();
        let recipient = recipient.clone();
        async move {
            client
                .get(url)
                .bearer_auth(token)
                .query(&[("address", recipient.as_str()), ("timeout", "10")])
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    });
    p.deliver(&recipient);
    let found = waiter.await.unwrap();
    assert_eq!(found["status"], "matched");
    let id = found["email"]["id"].as_i64().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !observed.contains("email_received") {
            observed.push_str(std::str::from_utf8(&events.next().await.unwrap().unwrap()).unwrap());
        }
    })
    .await
    .unwrap();
    let list: Value = client
        .get(format!("{base}/v1/emails"))
        .bearer_auth(&token)
        .query(&[
            ("address", recipient.as_str()),
            ("from", "sender"),
            ("subject", "verification"),
            ("limit", "1"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(list[0].get("raw_eml").is_none());
    let page: Value = client
        .get(format!("{base}/v1/emails?after_id={id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(page.as_array().unwrap().is_empty());
    assert_eq!(
        client
            .get(format!("{base}/v1/emails/{id}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/v1/emails/{id}/eml"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        p.raw
    );
    let range = client
        .get(format!("{base}/v1/emails/{id}/eml"))
        .bearer_auth(&token)
        .header("Range", "bytes=2-7")
        .send()
        .await
        .unwrap();
    assert_eq!(range.status(), 206);
    assert_eq!(range.bytes().await.unwrap().as_ref(), &p.raw[2..8]);
    let attachments: Value = client
        .get(format!("{base}/v1/emails/{id}/attachments"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(attachments.as_array().unwrap().len(), 1);
    assert_eq!(
        client
            .get(format!("{base}/v1/emails/{id}/attachments/1"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        b"hello attachment\n"
    );
    let conflict = p.command(&["listen"]).output().unwrap();
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("api"));
    assert_eq!(
        client
            .delete(format!("{base}/v1/addresses/{aid}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/v1/emails/{id}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .delete(format!("{base}/v1/emails/{id}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .delete(format!("{base}/v1/emails"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let o = p.command(&["api", "token", "regenerate"]).output().unwrap();
    assert!(o.status.success());
    assert!(!String::from_utf8_lossy(&o.stdout).contains(&token));
    assert_eq!(
        client
            .get(format!("{base}/v1/status"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let new_token = std::fs::read_to_string(&p.config.api_token_path).unwrap();
    assert!(new_token != token);
    assert_eq!(
        client
            .get(format!("{base}/v1/status"))
            .bearer_auth(&new_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    // Trigger an event: a revoked SSE must close before exposing this metadata.
    client
        .post(format!("{base}/v1/addresses"))
        .bearer_auth(&new_token)
        .json(&json!({"label":"after-rotation"}))
        .send()
        .await
        .unwrap();
    let remaining = tokio::time::timeout(Duration::from_secs(17), async {
        let mut rest = String::new();
        while let Some(chunk) = events.next().await {
            rest.push_str(std::str::from_utf8(&chunk.unwrap()).unwrap());
        }
        rest
    })
    .await
    .unwrap();
    assert!(!remaining.contains("after-rotation"));
}

#[tokio::test]
#[ignore = "starts isolated Go server and Rust API processes"]
async fn reconnect_end_to_end() {
    let mut p = TestProfile::new();
    let port = || {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let smtp = format!("127.0.0.1:{}", port());
    let https = format!("127.0.0.1:{}", port());
    p.config.smtp_dev_address = smtp.clone();
    p.config.server_url = format!("wss://{https}/ws");
    std::fs::write(&p.path, toml::to_string(&p.config).unwrap()).unwrap();
    let server = || {
        Process(
            Command::new(p.root.join("dist/ahrara-server"))
                .env("AHRARA_SMTP_BIND", &smtp)
                .env("AHRARA_HTTPS_BIND", &https)
                .env("AHRARA_TLS_CERT", &p.config.api_tls_cert_path)
                .env("AHRARA_TLS_KEY", &p.config.api_tls_key_path)
                .env("AHRARA_TLS_CA", p.config.tls_ca_path.as_ref().unwrap())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let go = server();
    let _api = p.spawn("api");
    let before = p.ready();
    let recipient = before["addresses"][0]["address"].as_str().unwrap();
    p.deliver(recipient);
    drop(go);
    let deadline = Instant::now() + Duration::from_secs(5);
    while p.run(&["status"])["connected"] == true {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let conflict = p.command(&["listen"]).output().unwrap();
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stderr).contains("api"));
    let client = reqwest::Client::builder()
        .add_root_certificate(
            reqwest::Certificate::from_pem(
                &std::fs::read(p.config.tls_ca_path.as_ref().unwrap()).unwrap(),
            )
            .unwrap(),
        )
        .build()
        .unwrap();
    let token = std::fs::read_to_string(&p.config.api_token_path).unwrap();
    let list: Value = client
        .get(format!("https://{}/v1/emails", p.config.api_bind))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    let _new_go = server();
    let after = p.ready();
    assert_eq!(after["route_id"], before["route_id"]);
    assert_eq!(after["addresses"], before["addresses"]);
    p.deliver(recipient);
    assert_eq!(p.run(&["status"])["emails_count"], 2);
}
