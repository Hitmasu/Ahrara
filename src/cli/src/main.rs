use ahrara::{
    auth,
    config::Config,
    core::{Core, Ownership},
};
use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::json;
use std::{
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    sync::Arc,
};
use zeroize::Zeroizing;

mod listener_ui;
mod palette;

#[derive(Parser)]
#[command(
    name = "ahrara",
    version,
    about = "Ephemeral email. Server streams, your computer stores."
)]
struct Cli {
    /// Use this config.toml; relative paths in it resolve beside the file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Print machine-readable JSON instead of the interactive or text output.
    #[arg(long, global = true)]
    json: bool,
    /// Disable background update notifications for this invocation.
    #[arg(long, global = true)]
    no_update_check: bool,
    /// Enable the local Web Page and API for this profile.
    #[arg(long, global = true)]
    web: bool,
    /// Use HTTPS for the shared Web, API and MCP server.
    #[arg(long, global = true)]
    https: bool,
    /// Require the local API bearer token for MCP connections (the default
    /// unless the config sets mcp_auth = false).
    #[arg(long, global = true)]
    mcp_auth: bool,
    /// Expose Web and MCP to the private LAN (HTTPS); MCP still requires a bearer token.
    #[arg(long, global = true)]
    lan: bool,
    /// Explicitly acknowledge LAN access without a Web password.
    #[arg(long, global = true, requires = "lan")]
    allow_no_password: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Receive email for this profile's Identity (the default command).
    Listen,
    /// Show connection, listener and local inbox status.
    Status,
    /// Print the installed version.
    Version,
    /// Check releases or update a registered direct installation.
    Update {
        /// Check only; never download or replace the executable.
        #[arg(long, conflicts_with = "register")]
        check: bool,
        /// Record the installer, or opt a manual binary into managed updates.
        /// Stop all existing instances before registering a manual installation.
        #[arg(long, value_enum)]
        register: Option<ahrara::update::InstallMethod>,
    },
    /// List, read, export and delete locally stored email.
    Inbox {
        #[command(subcommand)]
        command: Inbox,
    },
    /// Export or restore an encrypted inbox snapshot while the listener is stopped.
    Database {
        #[command(subcommand)]
        command: Database,
    },
    /// Create and manage the aliases of this profile's Identity.
    Address {
        #[command(subcommand)]
        command: Address,
    },
    /// Configure one local default custom domain and its external catch-all destination.
    Domain {
        #[command(subcommand)]
        command: Domain,
    },
    /// Show, export, register or replace the Identity Recovery Key.
    Auth {
        #[command(subcommand)]
        command: Auth,
    },
    /// Serve MCP over HTTP, or let an agent manage a shared local server over stdio.
    Mcp {
        #[arg(long, value_enum, default_value = "http")]
        transport: McpTransport,
        #[arg(long, hide = true)]
        managed: bool,
    },
    /// Manage Web preferences while the listener is stopped.
    Web {
        #[command(subcommand)]
        command: WebCommand,
    },
    /// Serve the local REST API, or manage its bearer token.
    Api {
        #[command(subcommand)]
        command: Option<ApiCommand>,
    },
    /// Print the generated API contract without loading an identity.
    #[command(hide = true)]
    Openapi,
}

#[derive(Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
enum McpTransport {
    Http,
    Stdio,
}

#[derive(Subcommand)]
enum Inbox {
    List(ahrara::api::EmailQuery),
    Show {
        id: i64,
        #[arg(long, default_value_t = 16384)]
        max_body_bytes: usize,
    },
    Eml {
        id: i64,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Clear,
    Delete {
        id: i64,
    },
    Attachments {
        id: i64,
    },
    Attachment {
        #[command(subcommand)]
        command: Attachment,
    },
}

#[derive(Subcommand)]
enum Attachment {
    Save {
        email_id: i64,
        attachment_id: String,
        destination: PathBuf,
    },
}

#[derive(Subcommand)]
enum Address {
    Create {
        #[arg(long)]
        label: Option<String>,
    },
    /// Receive mail for an address created elsewhere with this Identity, such as
    /// in the browser app.
    Add { address: String },
    List {
        #[arg(long, default_value_t = 0)]
        after_id: i64,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    Show {
        #[arg(allow_hyphen_values = true)]
        address_or_id: String,
    },
    Enable {
        #[arg(allow_hyphen_values = true)]
        address_or_id: String,
    },
    Disable {
        #[arg(allow_hyphen_values = true)]
        address_or_id: String,
    },
    Delete {
        #[arg(allow_hyphen_values = true)]
        address_or_id: String,
    },
}

#[derive(Subcommand)]
enum Domain {
    Set {
        domain: String,
        #[arg(long)]
        forward_to: String,
    },
    Show,
    Remove,
}

#[derive(Subcommand)]
enum Auth {
    Fingerprint,
    /// Register a Recovery Key only when this profile has no Identity.
    Set {
        #[arg(long)]
        stdin: bool,
    },
    Export {
        #[arg(long)]
        copy: bool,
    },
    /// Replace the Identity of a profile without an inbox. Reads the Recovery
    /// Key from a prompt or stdin, never from arguments visible to other processes.
    Import {
        #[arg(long)]
        stdin: bool,
        #[arg(long)]
        yes: bool,
    },
    Regenerate {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum Database {
    /// Write an encrypted snapshot to a new path; never overwrite an existing file.
    Export { path: PathBuf },
    /// Restore an encrypted snapshot using this profile's Recovery Key.
    Import {
        path: PathBuf,
        /// Explicitly replace the current inbox after validating the snapshot.
        #[arg(long)]
        replace: bool,
    },
}

#[derive(Subcommand)]
enum WebCommand {
    Enable,
    Disable,
    Password {
        #[arg(long)]
        stdin: bool,
        #[arg(long, conflicts_with = "stdin")]
        remove: bool,
        #[arg(long)]
        allow_no_password: bool,
    },
}

#[derive(Subcommand)]
enum ApiCommand {
    Token {
        #[command(subcommand)]
        command: Token,
    },
}

#[derive(Subcommand)]
enum Token {
    Show,
    Regenerate,
}

fn output(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn confirm(yes: bool, prompt: &str) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!("use --yes to confirm identity replacement")
    }

    eprint!("{prompt} [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !answer.trim().eq_ignore_ascii_case("y") {
        bail!("cancelled")
    }

    Ok(())
}

fn recovery_key(stdin: bool) -> Result<Zeroizing<String>> {
    if !stdin && io::stdin().is_terminal() {
        return Ok(Zeroizing::new(rpassword::prompt_password(
            "Recovery key: ",
        )?));
    }

    let mut secret = Zeroizing::new(String::new());
    io::stdin().lock().take(4097).read_to_string(&mut secret)?;
    if secret.len() > 4096 {
        bail!("Recovery key input is too large")
    }

    Ok(secret)
}

fn recovery(identity: &auth::Identity) {
    let key = identity.export();
    eprintln!(
        "Identity created.\n\nRecovery key:\n{}\n\nSave this key somewhere safe. Anyone with it can access your Ahrara identity.",
        key.as_str()
    );
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("Error: {e}");
        std::process::exit(1)
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Listen);
    if let Command::Update { check, register } = command {
        if let Some(method) = register {
            let executable = std::env::current_exe()?.canonicalize()?;
            ahrara::update::register(&executable, method)?;
            return output(&json!({"registered": true, "install_method": method}));
        }

        let status = ahrara::update::run(check).await?;
        return if cli.json {
            output(&status)
        } else {
            println!("{}", status.message);
            Ok(())
        };
    }

    let _installation_guard = ahrara::update::running_guard()?;
    if matches!(command, Command::Version) {
        println!("ahrara {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if matches!(command, Command::Openapi) {
        print!("{}", ahrara::api::openapi_yaml()?);
        return Ok(());
    }

    let mut cfg = Config::load(cli.config.as_deref())?;
    if cli.no_update_check {
        cfg.check_for_updates = false;
    }
    if cli.https || cli.mcp_auth {
        if !matches!(
            command,
            Command::Listen | Command::Mcp { .. } | Command::Api { command: None }
        ) {
            bail!("--https and --mcp-auth are listener options")
        }

        cfg.api_https |= cli.https;
        cfg.mcp_auth |= cli.mcp_auth;
    }
    if let Command::Mcp {
        transport: McpTransport::Stdio,
        managed,
    } = command
    {
        if managed || cli.web || cli.lan {
            bail!(
                "stdio reuses the profile's Web settings; --managed, --web and --lan require HTTP"
            )
        }

        ahrara::mcp_stdio::serve(cfg).await?;
        // Tokio's blocking stdin reader may outlive a disconnected HTTP peer.
        // All sessions and file locks have been closed before exiting here.
        std::process::exit(0);
    }
    if let Command::Auth {
        command: Auth::Set { stdin },
    } = command
    {
        let _guard = Ownership::acquire(&cfg, "auth set", "")?;
        if cfg.identity_path.exists() {
            bail!("Identity already exists; auth set never replaces an existing key")
        }

        let identity = auth::Identity::install_new(&recovery_key(stdin)?, &cfg)?;
        return output(&json!({
            "registered": true,
            "identity_fingerprint": identity.fingerprint()
        }));
    }
    if let Command::Database { command } = command {
        let _guard = Ownership::acquire(&cfg, "database", "")?;
        let identity = auth::Identity::load(&cfg).map_err(|_| {
            anyhow::anyhow!("Identity is required; register its Recovery Key with auth set --stdin")
        })?;
        let key = identity.database_key();
        match command {
            Database::Export { path } => {
                if !cfg.database_path.exists() {
                    bail!("database does not exist")
                }

                let storage = ahrara::storage::Storage::open(&cfg.database_path, &key)?;
                storage.export(path.clone()).await?;
                return output(&json!({
                    "exported": true,
                    "path": path
                }));
            }
            Database::Import { path, replace } => {
                ahrara::storage::Storage::import(&cfg.database_path, &path, &key, replace)?;
                return output(&json!({
                    "imported": true,
                    "path": cfg.database_path
                }));
            }
        }
    }
    if let Command::Auth {
        command: Auth::Import { stdin, yes },
    } = command
    {
        let _guard = Ownership::acquire(&cfg, "auth import", "")?;
        if cfg.database_path.try_exists()? {
            bail!("cannot replace Identity while an inbox exists; use a separate profile")
        }
        if cfg.identity_path.exists() {
            confirm(yes, "Replace the current Identity?")?;
        }

        auth::Identity::import(&recovery_key(stdin)?, &cfg)?;
        return output(&json!({
            "imported": true,
            "identity_fingerprint": auth::Identity::load(&cfg)?.fingerprint()
        }));
    }
    if let Command::Auth {
        command: Auth::Regenerate { yes },
    } = command
    {
        let _guard = Ownership::acquire(&cfg, "auth regenerate", "")?;
        if cfg.database_path.try_exists()? {
            bail!("cannot regenerate Identity while an inbox exists; use a separate profile")
        }

        confirm(
            yes,
            "Generate a new Identity? Save the old Recovery Key first.",
        )?;
        let identity = auth::Identity::regenerate(&cfg)?;
        if io::stdout().is_terminal() && !cli.json {
            recovery(&identity);
        }

        return output(&json!({
            "regenerated": true,
            "identity_fingerprint": identity.fingerprint()
        }));
    }
    if let Command::Api {
        command: Some(ApiCommand::Token { command }),
    } = command
    {
        match command {
            Token::Show => {
                let secret = Zeroizing::new(std::fs::read_to_string(&cfg.api_token_path)?);
                println!("{}", secret.as_str());
            }
            Token::Regenerate => {
                auth::generate_token(&cfg, true)?;
                println!("Local API token regenerated. Use `ahrara api token show` to export it.");
            }
        }

        return Ok(());
    }

    let silent = matches!(command, Command::Mcp { .. }) || cli.json || !io::stdout().is_terminal();
    let (core, created) = Core::open(cfg)?;
    if created && !silent {
        recovery(&core.identity);
    }
    if cli.web || cli.lan {
        if !matches!(
            command,
            Command::Listen | Command::Mcp { .. } | Command::Api { command: None }
        ) {
            bail!("--web and --lan are listener options")
        }

        let _guard =
            Ownership::acquire(&core.config, "web settings", &core.identity.fingerprint())?;
        if cli.lan {
            enable_lan(&core, cli.allow_no_password).await?;
        }

        core.update_interfaces("web", true, false, None).await?;
        core.update_interfaces("api", true, false, None).await?;
    }

    match command {
        Command::Web { command } => {
            let _guard =
                Ownership::acquire(&core.config, "web settings", &core.identity.fingerprint())?;
            match command {
                WebCommand::Enable => core.update_interfaces("web", true, false, None).await?,
                WebCommand::Disable => core.update_interfaces("web", false, false, None).await?,
                WebCommand::Password {
                    stdin,
                    remove,
                    allow_no_password,
                } => {
                    if remove {
                        core.update_interfaces("remove_password", false, allow_no_password, None)
                            .await?;
                    } else {
                        let password = if stdin {
                            let mut value = Zeroizing::new(String::new());
                            io::stdin().read_to_string(&mut value)?;

                            while value.ends_with(['\n', '\r']) {
                                value.pop();
                            }

                            value
                        } else {
                            read_web_password()?
                        };

                        core.update_interfaces("password", true, false, Some(password))
                            .await?;
                    }
                }
            }

            output(&json!({"updated": true}))
        }
        Command::Listen => listen(core, cli.json).await,
        Command::Status => output(&core.status().await?),
        Command::Mcp { managed, .. } => ahrara::mcp::serve(core, managed).await,
        Command::Api { command: None } => ahrara::api::serve(core).await,
        Command::Auth {
            command: Auth::Fingerprint,
        } => {
            println!("{}", core.identity.fingerprint());
            Ok(())
        }
        Command::Auth {
            command: Auth::Export { copy },
        } => {
            let key = core.identity.export();
            if copy {
                let mut clipboard = arboard::Clipboard::new()?;
                clipboard.set_text(key.as_str())?;
                #[cfg(target_os = "linux")]
                {
                    use arboard::SetExtLinux;
                    eprintln!(
                        "Recovery key copied. Keeping clipboard available for up to 60 seconds; paste it now."
                    );

                    clipboard
                        .set()
                        .wait_until(std::time::Instant::now() + std::time::Duration::from_secs(60))
                        .text(key.as_str())?;
                }

                #[cfg(not(target_os = "linux"))]
                eprintln!("Recovery key copied to clipboard.");
            } else {
                println!("{}", key.as_str());
            }

            Ok(())
        }
        Command::Domain { command } => match command {
            Domain::Set { domain, forward_to } => output(
                &core
                    .storage
                    .set_custom_domain(core.identity.fingerprint(), domain, forward_to)
                    .await?,
            ),
            Domain::Show => output(
                &core
                    .storage
                    .custom_domain(core.identity.fingerprint())
                    .await?,
            ),
            Domain::Remove => {
                core.storage
                    .remove_custom_domain(core.identity.fingerprint())
                    .await?;

                output(&json!({"removed": true}))
            }
        },
        Command::Address { command } => match command {
            Address::Create { label } => {
                let a = core.create_address(label).await?;
                if cli.json {
                    output(&a)
                } else {
                    println!("{}", a.address);
                    Ok(())
                }
            }
            Address::Add { address } => output(&core.add_address(address).await?),
            Address::List { after_id, limit } => output(
                &core
                    .storage
                    .addresses(core.identity.fingerprint(), after_id, limit, false)
                    .await?,
            ),
            Address::Show { address_or_id } => output(
                &core
                    .storage
                    .get_address(core.identity.fingerprint(), address_or_id)
                    .await?,
            ),
            Address::Enable { address_or_id } => {
                output(&core.change_address(address_or_id, Some(true)).await?)
            }
            Address::Disable { address_or_id } => {
                output(&core.change_address(address_or_id, Some(false)).await?)
            }
            Address::Delete { address_or_id } => {
                output(&core.change_address(address_or_id, None).await?)
            }
        },
        Command::Inbox { command } => match command {
            Inbox::List(q) => output(&core.storage.list(q.into(), None).await?),
            Inbox::Show { id, max_body_bytes } => {
                let e = core.get_email(id, max_body_bytes).await?;
                if cli.json {
                    return output(&e);
                }

                println!(
                    "From        {}\nTo          {}\nCc          {}\nSubject     {}\nDate        {}\nRecipient   {}\nAttachments {}\nHTML        {}\n\n{}",
                    ahrara::mail::terminal_safe(&e.metadata.from_header),
                    ahrara::mail::terminal_safe(&e.metadata.to_header),
                    ahrara::mail::terminal_safe(&e.cc),
                    ahrara::mail::terminal_safe(&e.metadata.subject),
                    ahrara::mail::terminal_safe(&ahrara::mail::local_date(
                        &e.date,
                        "%Y-%m-%d %H:%M:%S %:z"
                    )),
                    e.metadata.recipient,
                    e.attachments.len(),
                    e.html_available,
                    ahrara::mail::terminal_safe(&e.text_body)
                );

                Ok(())
            }
            Inbox::Eml { id, output: path } => {
                let (_, raw) = core.storage.email(id).await?;
                if let Some(path) = path {
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path)?;
                    file.write_all(&raw)?;
                    file.sync_all()?;
                } else {
                    if io::stdout().is_terminal() {
                        bail!("redirect stdout or use --output to export exact EML bytes")
                    };
                    io::stdout().write_all(&raw)?;
                }

                Ok(())
            }
            Inbox::Clear => output(&json!({"deleted": core.delete_emails(None).await?})),
            Inbox::Delete { id } => {
                output(&json!({"deleted": core.delete_emails(Some(id)).await?}))
            }
            Inbox::Attachments { id } => output(&core.get_email(id, 1).await?.attachments),
            Inbox::Attachment {
                command:
                    Attachment::Save {
                        email_id,
                        attachment_id,
                        destination,
                    },
            } => {
                let path = core
                    .save_attachment(email_id, attachment_id, &destination)
                    .await?;
                output(&json!({"path": path, "saved": true}))
            }
        },
        _ => unreachable!(),
    }
}

async fn listen(core: Arc<Core>, json_output: bool) -> Result<()> {
    let mut events = core.events.subscribe();
    let mut http = ahrara::api::HttpServer::new(core.clone(), false).await?;
    let mut state = core.state.subscribe();
    let interactive = !json_output
        && io::stdout().is_terminal()
        && std::env::var("TERM").as_deref() != Ok("dumb");
    let mut ui = interactive.then(listener_ui::ListenerUi::new).transpose()?;
    let (update_sender, mut update_notices) = tokio::sync::mpsc::channel(1);
    let update_task = if interactive && core.config.check_for_updates {
        Some(tokio::spawn(ahrara::update::notifications(update_sender)))
    } else {
        None
    };
    let mut updates_open = update_task.is_some();
    let mut input = ui.as_ref().and_then(listener_ui::ListenerUi::input_stream);
    let mut status = None;
    let mut stats = listener_ui::Stats::default();
    let mut tick = 0;
    let shutdown = ahrara::shutdown_signal();
    tokio::pin!(shutdown);
    // Keep startup in this select loop so connecting is visible and cancellable.
    // The same core still owns authentication, timeout, reconnect and shutdown.
    let result = async {
        if let Some(ui) = &mut ui {
            ui.refresh(&core).await?;
        }

        let startup = core.start("listen");
        tokio::pin!(startup);
        let mut repaint = tokio::time::interval(std::time::Duration::from_millis(125));
        repaint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            if let Some(ui) = &mut ui {
                let mut connection = state.borrow().clone();
                if status.is_none() && connection.state == "stopped" {
                    connection.state = "connecting".into();
                }

                stats.interfaces(&core);
                ui.draw(&connection, status.as_ref(), &stats, tick)?;
            }

            tokio::select! {
                _ = &mut shutdown => break,
                notice = update_notices.recv(), if updates_open => {
                    match notice {
                        Some(message) => if let Some(ui) = &mut ui { ui.update_notice(message); },
                        None => updates_open = false,
                    }
                },
                event = async { input.as_mut().unwrap().next().await }, if input.is_some() => {
                    let Some(event) = event else { input = None; continue; };
                    if let Some(ui) = &mut ui
                        && ui.handle_input(&core, &stats, event?).await? {
                        break;
                    }
                },
                started = &mut startup, if status.is_none() => {
                    let started = started?;
                    if ui.is_none() {
                        println!("{}", serde_json::to_string(&started)?);
                    }
                    status = Some(started);
                    if ui.is_some() { stats.refresh_address(&core).await?; }
                }
                _ = state.changed(), if ui.is_some() => {},
                _ = repaint.tick(), if ui.is_some() => { tick = tick.wrapping_add(1); },
                event = events.recv(), if status.is_some() => {
                    let event = match event {
                        Ok(event) => event,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            if let Some(ui) = &mut ui {
                                ui.email_received(&core).await?;
                                status = Some(core.status().await?);
                                stats.refresh_address(&core).await?;
                            }
                            continue;
                        }
                        Err(_) => break,
                    };
                    if matches!(event.kind.as_str(), "web_settings_changed" | "mcp_state_changed") && let Err(error)=http.sync().await {
                        *core.web.status.lock().unwrap()="Local server startup failed; check configuration".into();
                        if ui.is_none() { eprintln!("Local server: {error}"); }
                    }
                    if matches!(event.kind.as_str(),"address_created"|"address_deleted"|"address_updated"|"connection_state_changed") {
                        status=Some(core.status().await?);
                        if ui.is_some() { stats.refresh_address(&core).await?; }
                    }
                    if ui.is_none() {
                        println!("{}", serde_json::to_string(&event)?);
                    } else if let Some(ui) = &mut ui {
                        match event.kind.as_str() {
                            "email_received" => ui.email_received(&core).await?,
                            "email_deleted" | "emails_cleared" => ui.inbox_deleted(&core, event.data.get("id").and_then(|id|id.as_i64())).await?,
                            _ => {},
                        }
                    }
                }
            }
        }

        Ok(())
    }
    .await;
    if let Some(task) = update_task {
        task.abort();
    }

    http.stop().await;
    core.stop().await;
    if let Some(ui) = &mut ui {
        let final_state = ahrara::core::ConnectionState {
            state: if result.is_ok() { "stopped" } else { "failed" }.into(),
            ..Default::default()
        };

        ui.draw(&final_state, status.as_ref(), &stats, tick)?;
    }

    result
}

fn read_web_password() -> Result<Zeroizing<String>> {
    if !io::stdin().is_terminal() {
        bail!("use ahrara web password --stdin for non-interactive setup")
    }

    let password = Zeroizing::new(rpassword::prompt_password("New Web password: ")?);
    let confirmation = Zeroizing::new(rpassword::prompt_password("Confirm Web password: ")?);
    if *password != *confirmation {
        bail!("passwords do not match")
    }

    Ok(password)
}

async fn enable_lan(core: &Arc<Core>, mut acknowledged: bool) -> Result<()> {
    let settings = core.web.settings();
    if settings.password_hash.is_none() && !settings.lan_no_auth_acknowledged && !acknowledged {
        if !io::stdin().is_terminal() {
            bail!("LAN requires a password or explicit --allow-no-password acknowledgement")
        }

        eprintln!(
            "{}\n\n[c] Cancel (default)  [p] Set password  [y] Continue without authentication",
            ahrara::web_state::LAN_WARNING
        );
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        match answer.trim().to_ascii_lowercase().as_str() {
            "p" => {
                core.update_interfaces("password", true, false, Some(read_web_password()?))
                    .await?
            }
            "y" => acknowledged = true,
            _ => bail!("LAN activation cancelled"),
        }
    }

    core.update_interfaces("lan", true, acknowledged, None)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_addresses_may_start_with_a_hyphen() {
        // Base64url addresses can begin with `-`; clap must not read them as flags.
        for args in [
            &["ahrara", "inbox", "list", "--address", "-Vx@ahrara.dev"][..],
            &["ahrara", "address", "delete", "-Vx@ahrara.dev"][..],
            &["ahrara", "address", "show", "-Vx@ahrara.dev"][..],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
    }
}
