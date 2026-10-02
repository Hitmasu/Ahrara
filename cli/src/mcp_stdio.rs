//! Agent-owned stdio sessions attach to the same HTTP server and inbox as Web.
use crate::{
    config::{Config, private_dir, private_write},
    core::Ownership,
    web_state::WebState,
};
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use rmcp::transport::{
    Transport,
    async_rw::AsyncRwTransport,
    streamable_http_client::{StreamableHttpClientTransport, StreamableHttpClientTransportConfig},
};
use std::{
    fs::{File, OpenOptions},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    process::Stdio,
    time::Duration,
};
use tokio::{
    process::Command,
    time::{Instant, sleep, timeout},
};

fn lock(cfg: &Config, name: &str) -> Result<File> {
    private_dir(cfg.profile_dir())?;
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cfg.profile_dir().join(name))?)
}

async fn startup_lock(cfg: &Config) -> Result<File> {
    let file = lock(cfg, "mcp.start.lock")?;
    timeout(Duration::from_secs(15), async {
        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(file),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    sleep(Duration::from_millis(25)).await
                }
                Err(e) => return Err(e.into()),
            }
        }
    })
    .await
    .context("another agent is still starting Ahrara")?
}

/// Hold the startup lock during shutdown so a new session cannot join a dying server.
pub(crate) async fn idle(cfg: &Config) -> Result<File> {
    let sessions = lock(cfg, "mcp.sessions.lock")?;
    loop {
        sleep(Duration::from_secs(1)).await;
        let start = startup_lock(cfg).await?;
        match FileExt::try_lock_exclusive(&sessions) {
            Ok(()) => return Ok(start),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    }
}

pub async fn serve(mut cfg: Config) -> Result<()> {
    let start = startup_lock(&cfg).await?;
    let sessions = lock(&cfg, "mcp.sessions.lock")?;
    FileExt::try_lock_shared(&sessions)?;
    let existing = Ownership::current_http(&cfg)?.is_some();
    if existing {
        // Includes one-shot --https / --mcp-auth options of the running server.
        cfg = Config::load(Some(&cfg.profile_dir().join("http.config.toml")))?;
    }
    let settings = WebState::load(&cfg)?.settings();
    ensure!(settings.mcp_enabled, "MCP is disabled for this profile");
    let mut address: SocketAddr = cfg
        .api_bind
        .parse()
        .context("invalid local server address")?;
    if address.ip().is_unspecified() || settings.lan_enabled {
        address.set_ip(if address.is_ipv4() {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        });
    }
    ensure!(
        address.ip().is_loopback() && address.port() != 0,
        "stdio requires a local server on a fixed loopback port"
    );
    let mut child = None;
    if !existing {
        ensure!(
            Ownership::current(&cfg)?.is_none(),
            "this profile already has a listener; enable its MCP interface"
        );
        let path = cfg.profile_dir().join("mcp.config.toml");
        private_write(&path, toml::to_string(&cfg)?.as_bytes(), true)?;
        let log = cfg.profile_dir().join("mcp-server.log");
        private_write(&log, b"", true)?;
        let log = OpenOptions::new().append(true).open(log)?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("--config")
            .arg(path)
            .args(["mcp", "--managed"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        child = Some(command.spawn().context("could not start Ahrara")?);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if Ownership::current_http(&cfg)?.is_some()
            && tokio::net::TcpStream::connect(address).await.is_ok()
        {
            break;
        }
        if let Some(child) = &mut child
            && let Some(status) = child.try_wait()?
        {
            bail!(
                "Ahrara exited ({status}); see {}",
                cfg.profile_dir().join("mcp-server.log").display()
            );
        }
        ensure!(
            Instant::now() < deadline,
            "Ahrara startup timed out; see mcp-server.log in the profile directory"
        );
        sleep(Duration::from_millis(25)).await;
    }
    drop(start);
    // The detached server is owned by all live stdio leases, not by the first agent.
    if let Some(mut child) = child {
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
    }
    let https = cfg.api_https || settings.lan_enabled;
    let mut client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3));
    if https {
        for path in std::iter::once(&cfg.api_tls_cert_path).chain(cfg.tls_ca_path.iter()) {
            for cert in reqwest::Certificate::from_pem_bundle(&std::fs::read(path)?)? {
                client = client.add_root_certificate(cert);
            }
        }
    }
    let url = format!("{}://{address}/mcp", if https { "https" } else { "http" });
    let mut transport = StreamableHttpClientTransportConfig::with_uri(url);
    if cfg.mcp_auth || settings.lan_enabled {
        let token = std::fs::read_to_string(&cfg.api_token_path)?;
        crate::auth::validate_api_token(token.trim())?;
        transport = transport.auth_header(token.trim());
    }
    let mut remote = StreamableHttpClientTransport::with_client(client.build()?, transport);
    let mut local = AsyncRwTransport::new_server(tokio::io::stdin(), tokio::io::stdout());
    let result = loop {
        tokio::select! {
            _=crate::shutdown_signal()=>break Ok(()),
            message=local.receive()=>match message {
                Some(message)=>if let Err(e)=remote.send(message).await { break Err(e.into()); },
                None=>break Ok(()),
            },
            message=remote.receive()=>match message {
                Some(message)=>if let Err(e)=local.send(message).await { break Err(e.into()); },
                None=>break Err(anyhow::anyhow!("local MCP connection closed")),
            },
        }
    };
    let _ = timeout(Duration::from_secs(2), remote.close()).await;
    drop(sessions);
    result
}
