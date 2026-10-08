//! Real SMTP ingress, real Rust clients, isolated profiles and real TLS.
//! Run through make e2e / mcp-e2e / api-e2e after docker compose up --wait.
use ahrara::{auth, config::Config};
use futures_util::StreamExt;
use rustls::pki_types::{CertificateDer, pem::PemObject};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
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

#[test]
#[ignore = "requires make dev-certs; launches an isolated ingress"]
fn forwarding_end_to_end() {
    let mut p = TestProfile::new();
    let port = || {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    };
    let smtp_port = port();
    let wss_port = port();
    p.config.server_url = format!("wss://localhost:{wss_port}");
    p.smtp = format!("localhost:{smtp_port}");
    std::fs::write(&p.path, toml::to_string(&p.config).unwrap()).unwrap();
    let server_dir = tempfile::tempdir().unwrap();
    let _server = Process(
        Command::new(p.ingress_binary())
            .env("AHRARA_SMTP_BIND", format!("127.0.0.1:{smtp_port}"))
            .env("AHRARA_HTTPS_BIND", format!("127.0.0.1:{wss_port}"))
            .env("AHRARA_TLS_CERT", &p.config.api_tls_cert_path)
            .env("AHRARA_TLS_KEY", &p.config.api_tls_key_path)
            .env(
                "AHRARA_TRANSPORT_KEY_FILE",
                p.root.join(".dev/certs/transport.key"),
            )
            .env("AHRARA_TRUSTED_EMAIL_ROUTING_CIDRS", "127.0.0.1/32")
            .current_dir(server_dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let destination = p.run(&["address", "create"]);
    let target = destination["address"].as_str().unwrap();
    p.run(&["domain", "set", "ab.com", &format!("--forward-to={target}")]);
    let public = p.run(&["address", "create"]);

    assert!(public["address"].as_str().unwrap().ends_with("@ab.com"));
    assert_eq!(public["forwarding_address"], target);
    let listener = Process(
        p.command(&["--web", "listen"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    p.ready();
    // The envelope deliberately has no online Identity. Only the trusted header
    // selects the actual destination; neither this envelope nor To can select it.
    let envelope = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@ahrara.dev";
    let raw = format!("From: sender@example.test\r\nTo: signup@ab.com\r\nX-Forwarded-To: {target}\r\nSubject: Custom domain\r\n\r\n.first\r\n..second\r\n").into_bytes();
    std::fs::write(&p.eml, &raw).unwrap();
    p.deliver(envelope);
    let list = p.run(&[
        "inbox",
        "list",
        &format!("--address={}", public["address"].as_str().unwrap()),
    ]);

    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["recipient"], target);
    assert_eq!(list[0]["to"], "signup@ab.com");
    let email = p.run(&["inbox", "show", "1"]);

    assert_eq!(email["forwarded_to"], target);
    let exported = p._dir.path().join("exported.eml");

    assert!(
        p.command(&["inbox", "eml", "1", "--output", exported.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(std::fs::read(exported).unwrap(), raw);
    std::fs::write(&p.eml, "To: signup@ab.com\r\n\r\nNo routing header\r\n").unwrap();

    assert!(p.send(envelope, None, 0).is_err());
    assert_eq!(p.run(&["status"])["emails_count"], 1);
    assert_eq!(std::fs::read_dir(server_dir.path()).unwrap().count(), 0);
    drop(listener);
    let mut mcp = McpClient::new(&p);
    let created = mcp.tool("ahrara_create_address", json!({}));

    assert!(created["address"].as_str().unwrap().ends_with("@ab.com"));
    assert_eq!(created["forwarding_address"], target);
    let found = mcp.tool(
        "ahrara_wait_for_email",
        json!({
            "address": created["address"],
            "after_id": 0,
            "timeout_seconds": 1
        }),
    );

    assert_eq!(found["email"]["recipient"], target);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let token =
            zeroize::Zeroizing::new(std::fs::read_to_string(&p.config.api_token_path).unwrap());
        let client = reqwest::Client::builder()
            .add_root_certificate(
                reqwest::Certificate::from_pem(
                    &std::fs::read(p.config.tls_ca_path.as_ref().unwrap()).unwrap(),
                )
                .unwrap(),
            )
            .build()
            .unwrap();
        let response = client
            .post(format!("https://{}/v1/addresses", p.config.api_bind))
            .bearer_auth(token.as_str())
            .json(&json!({}))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), 201);
        let created: Value = response.json().await.unwrap();

        assert!(created["address"].as_str().unwrap().ends_with("@ab.com"));
        assert_eq!(created["forwarding_address"], target);
    });
}

#[test]
#[ignore = "requires make dev-certs; launches an isolated ingress"]
fn email_routing_end_to_end() {
    let sink = "relay@ingest.ahrara.dev";
    for verification in [false, true] {
        let mut p = TestProfile::new();
        let port = || {
            std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port()
        };
        let smtp_port = port();
        let wss_port = port();
        p.config.server_url = format!("wss://localhost:{wss_port}");
        p.smtp = format!("localhost:{smtp_port}");
        std::fs::write(&p.path, toml::to_string(&p.config).unwrap()).unwrap();
        let destination = p.run(&["address", "create"]);
        let target = destination["address"].as_str().unwrap();
        let server_dir = tempfile::tempdir().unwrap();
        let _server = Process(
            Command::new(p.ingress_binary())
                .env("AHRARA_SMTP_BIND", format!("127.0.0.1:{smtp_port}"))
                .env("AHRARA_HTTPS_BIND", format!("127.0.0.1:{wss_port}"))
                .env("AHRARA_TLS_CERT", &p.config.api_tls_cert_path)
                .env("AHRARA_TLS_KEY", &p.config.api_tls_key_path)
                .env(
                    "AHRARA_TRANSPORT_KEY_FILE",
                    p.root.join(".dev/certs/transport.key"),
                )
                .env("AHRARA_TRUSTED_EMAIL_ROUTING_CIDRS", "127.0.0.1/32")
                .env("AHRARA_EMAIL_ROUTING_ADDRESS", sink)
                .env(
                    "AHRARA_EMAIL_ROUTING_VERIFICATION_RECIPIENT",
                    if verification { target } else { "" },
                )
                .current_dir(server_dir.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let _listener = p.spawn("listen");
        p.ready();
        std::fs::write(
            &p.eml,
            "Subject: Verify your destination\r\n\r\nSynthetic verification email.\r\n",
        )
        .unwrap();
        assert_eq!(p.send(sink, None, 0).is_ok(), verification);
        std::fs::write(
            &p.eml,
            format!("X-Forwarded-To: {sink}\r\nX-Forwarded-For: {target} {sink}\r\nTo: unrelated@example.test\r\n\r\nNative forwarding with a different visible recipient.\r\n"),
        )
        .unwrap();
        assert!(p.send(sink, None, 0).is_ok());
        assert!(
            p.send(target, None, 0).is_err(),
            "only the configured sink may accept trusted forwards"
        );
        assert!(p.send("other@ingest.ahrara.dev", None, 0).is_err());
        assert_eq!(
            p.run(&["status"])["emails_count"],
            if verification { 2 } else { 1 }
        );
        let list = p.run(&["inbox", "list"]);
        assert_eq!(list[0]["recipient"], target);
        if !verification {
            std::fs::write(
                &p.eml,
                format!("X-Forwarded-To: {sink}\r\nX-Forwarded-For: {target} {sink}\r\nX-Forwarded-For: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@ahrara.dev {sink}\r\n\r\nAmbiguous forwarding.\r\n"),
            )
            .unwrap();
            assert!(p.send(sink, None, 0).is_err());
            assert_eq!(p.run(&["status"])["emails_count"], 1);
        }
        assert_eq!(std::fs::read_dir(server_dir.path()).unwrap().count(), 0);
    }
}

struct TestProfile {
    _dir: tempfile::TempDir,
    config: Config,
    smtp: String,
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
            .parent()
            .unwrap()
            .to_owned();
        let _dir = tempfile::tempdir().unwrap();
        let dir = _dir.path();
        let profile = root.join(".dev/profile/config.toml");
        let dev = Config::load(Some(&profile)).expect("run make dev-certs");
        // make dev-certs records the SMTP endpoint; the client ignores this key.
        let smtp = std::env::var("AHRARA_TEST_SMTP_ADDRESS").unwrap_or_else(|_| {
            toml::from_str::<toml::Table>(&std::fs::read_to_string(&profile).unwrap())
                .unwrap()
                .get("smtp_dev_address")
                .and_then(|value| value.as_str())
                .unwrap_or("localhost:2525")
                .into()
        });
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let config = Config {
            server_url: std::env::var("AHRARA_TEST_SERVER_URL").unwrap_or(dev.server_url),
            server_public_key: std::env::var("AHRARA_TEST_SERVER_PUBLIC_KEY")
                .ok()
                .or(dev.server_public_key)
                // dev-certs preserves older profiles that predate Noise pins.
                // Each isolated test profile can use the generated public pin.
                .or_else(|| {
                    std::fs::read_to_string(root.join(".dev/certs/transport-public.txt"))
                        .ok()
                        .map(|pin| pin.trim().to_owned())
                }),
            database_path: dir.join("ahrara.db"),
            identity_path: dir.join("identity.key"),
            api_token_path: dir.join("api.token"),
            api_https: true,
            mcp_auth: true,
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
            smtp,
            path,
            root,
            eml,
            raw,
        }
    }

    fn ingress_binary(&self) -> PathBuf {
        std::env::var_os("AHRARA_TEST_INGRESS_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| self.root.join("dist/ahrara-server"))
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

    fn send(&self, to: &str, file: Option<&Path>, size: usize) -> anyhow::Result<()> {
        fn exchange(
            stream: &mut BufReader<impl Read + Write>,
            command: &str,
            code: &str,
        ) -> anyhow::Result<()> {
            stream.get_mut().write_all(command.as_bytes())?;
            stream.get_mut().flush()?;

            loop {
                let mut line = String::new();
                stream.read_line(&mut line)?;
                anyhow::ensure!(line.starts_with(code), "SMTP expected {code}: {line}");
                if line.as_bytes().get(3) != Some(&b'-') {
                    return Ok(());
                }
            }
        }

        let socket = TcpStream::connect(&self.smtp)?;
        socket.set_read_timeout(Some(Duration::from_secs(90)))?;
        socket.set_write_timeout(Some(Duration::from_secs(90)))?;
        let mut stream = BufReader::new(socket);
        exchange(&mut stream, "", "220")?;
        exchange(&mut stream, "EHLO localhost\r\n", "250")?;
        exchange(&mut stream, "STARTTLS\r\n", "220")?;
        let ca = std::fs::read(self.config.tls_ca_path.as_ref().unwrap())?;
        let mut roots = rustls::RootCertStore::empty();

        for cert in CertificateDer::pem_slice_iter(&ca) {
            roots.add(cert?)?;
        }

        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let host = self.smtp.rsplit_once(':').unwrap().0;
        let connection = rustls::ClientConnection::new(
            std::sync::Arc::new(tls),
            rustls::pki_types::ServerName::try_from(host.trim_matches(['[', ']']).to_owned())?,
        )?;
        let mut stream = BufReader::new(rustls::StreamOwned::new(connection, stream.into_inner()));
        exchange(&mut stream, "EHLO localhost\r\n", "250")?;
        let size = if size == 0 {
            String::new()
        } else {
            format!(" SIZE={size}")
        };
        exchange(
            &mut stream,
            &format!("MAIL FROM:<sender@example.test>{size}\r\n"),
            "250",
        )?;

        for recipient in to.split(',') {
            exchange(&mut stream, &format!("RCPT TO:<{recipient}>\r\n"), "250")?;
        }

        exchange(&mut stream, "DATA\r\n", "354")?;
        let raw = std::fs::read(file.unwrap_or(&self.eml))?;

        for line in raw.split_inclusive(|byte| *byte == b'\n') {
            if line.starts_with(b".") {
                stream.get_mut().write_all(b".")?;
            }

            stream.get_mut().write_all(line)?;
        }
        if !raw.ends_with(b"\r\n") {
            stream.get_mut().write_all(b"\r\n")?;
        }

        exchange(&mut stream, ".\r\n", "250")?;
        exchange(&mut stream, "QUIT\r\n", "221")
    }

    fn deliver(&self, to: &str) {
        self.send(to, None, 0).expect("SMTP delivery failed");
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

    assert!(rejected.unwrap_err().to_string().contains("550"));
    assert_eq!(p.run(&["inbox", "list"]).as_array().unwrap().len(), 3);
    p.run(&["address", "disable", recipients[0]]);

    assert!(p.send(recipients[0], None, 0).is_err());
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

    assert!(sent.is_ok(), "exact limit: {sent:?}");
    boundary.insert(0, b'x');
    std::fs::write(&file, &boundary).unwrap();

    assert!(p.send(recipients[0], Some(&file), 0).is_err());
    assert!(p.send(recipients[0], Some(&file), boundary.len()).is_err());
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

#[test]
#[ignore = "requires Docker SMTP/WSS server"]
fn mcp_stdio_end_to_end() {
    use rmcp::{ServiceExt, model::CallToolRequestParams, transport::async_rw::AsyncRwTransport};
    let p = TestProfile::new();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (mut process, client) = runtime.block_on(async {
        let mut command =
            tokio::process::Command::from(p.command(&["mcp", "--transport", "stdio"]));
        let mut process = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let transport = AsyncRwTransport::new_client(
            process.stdout.take().unwrap(),
            process.stdin.take().unwrap(),
        );
        let client = tokio::time::timeout(Duration::from_secs(15), ().serve(transport))
            .await
            .unwrap()
            .unwrap();

        (process, client)
    });
    p.ready();
    let tool = |name: &str, arguments: Value| {
        let result = runtime
            .block_on(
                client.call_tool(
                    CallToolRequestParams::new(name.to_owned())
                        .with_arguments(arguments.as_object().unwrap().clone()),
                ),
            )
            .unwrap();
        assert_ne!(result.is_error, Some(true));
        result.structured_content.unwrap()
    };

    assert_eq!(tool("ahrara_start", json!({}))["connected"], true);
    let address = tool("ahrara_create_address", json!({"label": "Stdio"}));
    let recipient = address["address"].as_str().unwrap();
    p.deliver(recipient);
    let received = tool(
        "ahrara_wait_for_email",
        json!({"address": recipient, "timeout_seconds": 5}),
    );

    assert_eq!(received["status"], "matched");
    let id = received["email"]["id"].as_i64().unwrap();
    let eml = tool("ahrara_read_eml", json!({"id": id}));

    assert_eq!(eml["content"].as_str().unwrap().as_bytes(), p.raw);
    runtime.block_on(async {
        client.cancel().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), process.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );

        tokio::time::timeout(Duration::from_secs(5), async {
            while ahrara::core::Ownership::current_http(&p.config)
                .unwrap()
                .is_some()
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
    });
}

// Speak JSON-RPC over real HTTP(S), including negotiated sessions and SSE responses.
struct McpClient {
    _process: Process,
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
    url: String,
    token: Option<zeroize::Zeroizing<String>>,
    session: Option<String>,
    protocol_version: Option<String>,
    pending: Option<reqwest::Response>,
    next: u64,
}

impl McpClient {
    fn new(p: &TestProfile) -> Self {
        let process = p.spawn("mcp");
        p.ready();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::builder()
            .add_root_certificate(
                reqwest::Certificate::from_pem(
                    &std::fs::read(p.config.tls_ca_path.as_ref().unwrap()).unwrap(),
                )
                .unwrap(),
            )
            .build()
            .unwrap();
        let mut c = Self {
            _process: process,
            runtime,
            client,
            url: format!(
                "{}://{}/mcp",
                if p.config.api_https { "https" } else { "http" },
                p.config.api_bind
            ),
            token: p.config.mcp_auth.then(|| {
                zeroize::Zeroizing::new(std::fs::read_to_string(&p.config.api_token_path).unwrap())
            }),
            session: None,
            protocol_version: None,
            pending: None,
            next: 0,
        };
        let initialized = c.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "ahrara-tests", "version": "1"}
            }),
        );

        assert_eq!(initialized["protocolVersion"], "2025-03-26");
        c.protocol_version = Some(initialized["protocolVersion"].as_str().unwrap().into());
        c.write(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    fn post(&self, v: Value) -> reqwest::Response {
        let mut request = self
            .client
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .json(&v);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token.as_str());
        }
        if let Some(version) = &self.protocol_version {
            request = request.header("MCP-Protocol-Version", version);
        }
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session);
        }

        let response = self.runtime.block_on(request.send()).unwrap();

        assert!(
            response.status().is_success(),
            "MCP HTTP status: {}",
            response.status()
        );

        response
    }

    fn write(&mut self, v: Value) {
        let response = self.post(v);
        if response.status() == 200 {
            self.pending = Some(response);
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let response = self.post(json!({
            "jsonrpc": "2.0",
            "id": self.next,
            "method": method,
            "params": params
        }));
        if let Some(id) = response.headers().get("mcp-session-id") {
            self.session = Some(id.to_str().unwrap().into());
        }

        let text = self.runtime.block_on(response.text()).unwrap();

        assert!(
            !text.contains("ahrara_sk_v1_")
                && self
                    .token
                    .as_ref()
                    .is_none_or(|token| !text.contains(token.as_str()))
        );
        let value: Value = text
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
            .find(|v| v["id"] == self.next)
            .expect("MCP response event");

        assert!(value.get("error").is_none(), "MCP protocol error: {value}");
        value["result"].clone()
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert_ne!(r["isError"], true, "{r}");
        r["structuredContent"].clone()
    }
}

#[test]
#[ignore = "requires Docker SMTP/WSS server"]
fn mcp_end_to_end() {
    for secure in [false, true] {
        let mut p = TestProfile::new();
        p.config.api_https = secure;
        p.config.mcp_auth = secure;
        std::fs::write(&p.path, toml::to_string(&p.config).unwrap()).unwrap();
        let mut c = McpClient::new(&p);
        let tools = c.request("tools/list", json!({}));

        assert_eq!(tools["tools"].as_array().unwrap().len(), 17);
        let start = c.tool("ahrara_start", json!({}));

        assert_eq!(start["connected"], true);
        assert_eq!(start["addresses"].as_array().unwrap().len(), 1);
        let address = c.tool("ahrara_create_address", json!({"label": "MCP"}));
        let recipient = address["address"].as_str().unwrap();

        assert_eq!(
            c.tool("ahrara_start", json!({}))["route_id"],
            start["route_id"]
        );
        c.tool("ahrara_get_address", json!({"address_or_id": recipient}));
        c.tool(
            "ahrara_disable_address",
            json!({"address_or_id": recipient}),
        );
        c.tool("ahrara_enable_address", json!({"address_or_id": recipient}));

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
            json!({"address": recipient, "timeout_seconds": 5}),
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
        c.tool("ahrara_get_email", json!({"id": id}));
        let eml = c.tool("ahrara_read_eml", json!({"id": id}));

        assert_eq!(eml["encoding"], "utf8");
        assert_eq!(eml["content"].as_str().unwrap().as_bytes(), p.raw);
        assert_eq!(
            c.tool("ahrara_list_attachments", json!({"id":id}))["attachments"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let saved = c.tool(
            "ahrara_save_attachment",
            json!({
                "email_id": id,
                "attachment_id": "1",
                "filename": "saved.txt"
            }),
        );

        assert_eq!(
            std::path::Path::new(saved["path"].as_str().unwrap()),
            p._dir
                .path()
                .join("attachments")
                .canonicalize()
                .unwrap()
                .join("saved.txt")
        );

        assert_eq!(
            c.tool(
                "ahrara_wait_for_email",
                json!({"address":recipient,"after_id":id,"timeout_seconds":1})
            )["status"],
            "timeout"
        );
        c.tool("ahrara_delete_email", json!({"id": id}));
        // A cancelled wait must not block subsequent requests or stop the listener.
        c.next += 1;
        let cancelled = c.next;
        c.write(json!({
            "jsonrpc": "2.0",
            "id": cancelled,
            "method": "tools/call",
            "params": {
                "name": "ahrara_wait_for_email",
                "arguments": {"after_id": id, "timeout_seconds": 300}
            }
        }));
        std::thread::sleep(Duration::from_millis(20));
        c.write(json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": cancelled, "reason": "test cancellation"}
        }));

        assert_eq!(c.tool("ahrara_status", json!({}))["connected"], true);
        c.tool("ahrara_clear_inbox", json!({}));
        c.tool("ahrara_delete_address", json!({"address_or_id": recipient}));
        c.tool("ahrara_stop", json!({}));

        assert_eq!(c.tool("ahrara_status", json!({}))["connected"], false);
        drop(c);
        let _listener = p.spawn("listen");

        assert_eq!(p.ready()["route_id"], start["route_id"]);
    }
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
        .json(&json!({"label": "API"}))
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
        .json(&json!({"label": "after-rotation"}))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real TLS, kill/restart and network blackhole; up to 5 minutes"]
async fn reconnect_end_to_end() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
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
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    let (fault, _) = tokio::sync::watch::channel(0u8);
    let accepted = Arc::new(AtomicUsize::new(0));
    let proxy = tokio::spawn({
        let fault = fault.clone();
        let accepted = accepted.clone();
        let upstream = https.clone();
        async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (mut incoming, _) = proxy_listener.accept().await.unwrap();
                let upstream = upstream.clone();
                let mut events = fault.subscribe();
                accepted.fetch_add(1, Ordering::SeqCst);
                connections.spawn(async move {
                    let Ok(mut outgoing) = tokio::net::TcpStream::connect(upstream).await else {
                        return;
                    };
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing) => {},
                        _ = events.changed() => {
                            let mode = *events.borrow_and_update();
                            if mode == 2 {
                                // Keep both TCP sockets established but stop forwarding.
                                // New TCP connections still reach the real TLS Rust server.
                                std::future::pending::<()>().await;
                            }
                        }
                    }

                    drop((incoming, outgoing));
                });
            }
        }
    });
    p.smtp = smtp.clone();
    p.config.server_url = format!("wss://{proxy_address}");
    std::fs::write(&p.path, toml::to_string(&p.config).unwrap()).unwrap();
    let server = || {
        Process(
            Command::new(p.ingress_binary())
                .env("AHRARA_SMTP_BIND", &smtp)
                .env("AHRARA_HTTPS_BIND", &https)
                .env("AHRARA_TLS_CERT", &p.config.api_tls_cert_path)
                .env("AHRARA_TLS_KEY", &p.config.api_tls_key_path)
                .env(
                    "AHRARA_TRANSPORT_KEY_FILE",
                    p.root.join(".dev/certs/transport.key"),
                )
                .env("AHRARA_TLS_CA", p.config.tls_ca_path.as_ref().unwrap())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let ingress = server();
    let _api = p.spawn("api");
    let before = p.ready();
    let recipient = before["addresses"][0]["address"].as_str().unwrap();
    p.deliver(recipient);
    drop(ingress);
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
    let _new_ingress = server();
    let after = p.ready();

    assert_eq!(after["route_id"], before["route_id"]);
    assert_eq!(after["addresses"], before["addresses"]);
    p.deliver(recipient);

    assert_eq!(p.run(&["status"])["emails_count"], 2);

    let connections_before = accepted.load(Ordering::SeqCst);
    fault.send(1).unwrap(); // Abrupt TCP close, without a WS close frame.
    let deadline = Instant::now() + Duration::from_secs(35);

    while accepted.load(Ordering::SeqCst) == connections_before
        || p.run(&["status"])["connected"] != true
    {
        assert!(Instant::now() < deadline, "TCP close did not reconnect");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    p.deliver(recipient);

    assert_eq!(p.run(&["status"])["emails_count"], 3);

    fault.send(2).unwrap();
    let deadline = Instant::now() + Duration::from_secs(150);

    while p.run(&["status"])["connected"] == true {
        assert!(
            Instant::now() < deadline,
            "blackhole not detected by PONG timeout"
        );

        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let deadline = Instant::now() + Duration::from_secs(260);
    let restored = loop {
        let status = p.run(&["status"]);
        assert_ne!(
            status["status"], "failed",
            "reconnect gave up while old socket awaited expiry: {status}"
        );
        assert_ne!(
            status["status"], "stopped",
            "listener exited during network recovery: {status}"
        );
        if status["connected"] == true {
            break status;
        }

        assert!(
            Instant::now() < deadline,
            "no reconnect after stale Identity expiry"
        );

        tokio::time::sleep(Duration::from_millis(250)).await;
    };

    assert_eq!(restored["route_id"], before["route_id"]);
    p.deliver(recipient);

    assert_eq!(p.run(&["status"])["emails_count"], 4);
    proxy.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker SMTP/WSS server"]
async fn web_end_to_end() {
    let p = TestProfile::new();
    p.run(&["address", "create"]);
    let mut listener = Process(
        p.command(&[])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    p.ready();
    let conflict = p.command(&["listen"]).output().unwrap();

    assert!(!conflict.status.success());
    assert!(
        String::from_utf8_lossy(&conflict.stderr).contains("listener_owned"),
        "a Web listener conflict must identify its owner"
    );

    let ca = reqwest::Certificate::from_pem(
        &std::fs::read(p.config.tls_ca_path.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    let client = reqwest::Client::builder()
        .add_root_certificate(ca.clone())
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let base = format!("https://{}", p.config.api_bind);
    let page = client.get(&base).send().await.unwrap();

    assert_eq!(page.status(), 200);
    let page = page.text().await.unwrap();
    let csrf = page
        .split("name=\"csrf\" value=\"")
        .nth(1)
        .map(|s| s.split('"').next().unwrap().to_owned());
    // Empty inbox has no mutation forms; the addresses page always has one.
    let csrf = match csrf {
        Some(value) => value,
        None => {
            let text = client
                .get(format!("{base}/addresses"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();

            text.split("name=\"csrf\" value=\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .to_owned()
        }
    };

    assert_eq!(
        client
            .post(format!("{base}/addresses"))
            .form(&[("csrf", csrf.as_str()), ("label", "Web E2E")])
            .send()
            .await
            .unwrap()
            .status(),
        303
    );
    let aliases = p.run(&["address", "list"]);
    let address = aliases
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["label"] == "Web E2E")
        .unwrap()["address"]
        .as_str()
        .unwrap();
    let response = client.get(format!("{base}/events")).send().await.unwrap();

    assert_eq!(response.status(), 200);
    let mut events = response.bytes_stream();
    p.deliver(address);
    let mut data = String::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !data.contains("event: email_received") {
            data.push_str(&String::from_utf8_lossy(
                &events.next().await.unwrap().unwrap(),
            ));
        }
    })
    .await
    .unwrap();
    let inbox = client
        .get(&base)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(inbox.contains("Verification code 123456"));
    let email = p.run(&["inbox", "list"]);
    let id = email[0]["id"].as_i64().unwrap();
    let detail = client
        .get(format!("{base}/emails/{id}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(detail.contains("Attachments"));
    assert!(detail.contains("sandbox"));
    assert!(detail.contains("Your code is 123456."));
    let attachment = client
        .get(format!("{base}/emails/{id}/attachments/1"))
        .send()
        .await
        .unwrap();

    assert_eq!(attachment.status(), 200);
    assert!(
        attachment.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    assert!(!attachment.bytes().await.unwrap().is_empty());
    assert_eq!(
        client
            .get(format!("{base}/emails/{id}/eml"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        p.raw.as_slice()
    );
    assert_eq!(
        client
            .post(format!("{base}/emails/{id}/delete"))
            .form(&[("csrf", csrf.as_str()), ("confirm", "true")])
            .send()
            .await
            .unwrap()
            .status(),
        303
    );
    assert!(p.run(&["inbox", "list"]).as_array().unwrap().is_empty());
    assert_eq!(
        client
            .post(format!("{base}/settings"))
            .form(&[
                ("csrf", csrf.as_str()),
                ("action", "lan"),
                ("enabled", "true")
            ])
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let password = ahrara::web_state::random_secret();

    assert_eq!(
        client
            .post(format!("{base}/settings"))
            .form(&[
                ("csrf", csrf.as_str()),
                ("action", "password"),
                ("password", password.as_str()),
                ("password_confirm", password.as_str())
            ])
            .send()
            .await
            .unwrap()
            .status(),
        303
    );
    assert_eq!(client.get(&base).send().await.unwrap().status(), 303);
    let login = client
        .post(format!("{base}/login"))
        .form(&[("csrf", csrf.as_str()), ("password", password.as_str())])
        .send()
        .await
        .unwrap();

    assert_eq!(login.status(), 303);
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    assert_eq!(
        client
            .post(format!("{base}/settings"))
            .header("cookie", &cookie)
            .form(&[
                ("csrf", csrf.as_str()),
                ("action", "lan"),
                ("enabled", "true")
            ])
            .send()
            .await
            .unwrap()
            .status(),
        303
    );
    tokio::time::sleep(Duration::from_millis(1400)).await;
    let ip = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    ip.connect("192.0.2.1:9").unwrap();
    let ip = ip.local_addr().unwrap().ip();
    let port = p
        .config
        .api_bind
        .parse::<std::net::SocketAddr>()
        .unwrap()
        .port();
    // Resolve localhost to the LAN interface so the existing development SAN remains valid.
    let lan = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(ca)
        .redirect(reqwest::redirect::Policy::none())
        .resolve("localhost", std::net::SocketAddr::new(ip, port))
        .build()
        .unwrap();
    let lan_url = format!("https://localhost:{port}");

    assert_eq!(lan.get(&lan_url).send().await.unwrap().status(), 303);
    assert_eq!(
        lan.get(&lan_url)
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{base}/settings"))
            .header("cookie", &cookie)
            .form(&[
                ("csrf", csrf.as_str()),
                ("action", "api"),
                ("enabled", "true")
            ])
            .send()
            .await
            .unwrap()
            .status(),
        303
    );
    let api_token = std::fs::read_to_string(&p.config.api_token_path).unwrap();

    assert_eq!(
        lan.get(format!("{lan_url}/v1/status"))
            .bearer_auth(&api_token)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let next = ahrara::web_state::random_secret();

    assert_eq!(
        client
            .post(format!("{base}/settings"))
            .header("cookie", &cookie)
            .form(&[
                ("csrf", csrf.as_str()),
                ("action", "password"),
                ("password", next.as_str()),
                ("password_confirm", next.as_str())
            ])
            .send()
            .await
            .unwrap()
            .status(),
        303
    );
    assert_eq!(
        client
            .get(&base)
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        303
    );
    let login = client
        .post(format!("{base}/login"))
        .form(&[("csrf", csrf.as_str()), ("password", next.as_str())])
        .send()
        .await
        .unwrap();

    assert_eq!(login.status(), 303);
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let values = [
        ("csrf", csrf.as_str()),
        ("action", "remove_password"),
        ("confirm", "true"),
    ];

    assert_eq!(
        client
            .post(format!("{base}/settings"))
            .header("cookie", &cookie)
            .form(&values)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let mut values = values.to_vec();
    values.push(("acknowledge", "true"));

    assert_eq!(
        client
            .post(format!("{base}/settings"))
            .header("cookie", &cookie)
            .form(&values)
            .send()
            .await
            .unwrap()
            .status(),
        303
    );
    assert_eq!(client.get(&base).send().await.unwrap().status(), 200);
    drop(events);
    listener.0.kill().unwrap();
    listener.0.wait().unwrap();
    let restarted = p.spawn("listen");
    p.ready();

    assert_eq!(client.get(&base).send().await.unwrap().status(), 200);
    drop(restarted);
    println!(
        "Web E2E: HTTPS, shared process, address creation, SMTP/SSE, content, attachment, exact EML, delete, LAN login, password reset, revocation and persisted no-auth acknowledgement passed."
    );
}
