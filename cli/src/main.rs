use ahrara::{
    auth,
    config::Config,
    core::{Core, Ownership},
    storage::Filters,
};
use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use std::{
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    sync::Arc,
};
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    name = "ahrara",
    version,
    about = "Ephemeral email. Server streams, your computer stores."
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    Listen,
    Status,
    Version,
    Inbox {
        #[command(subcommand)]
        command: Inbox,
    },
    Address {
        #[command(subcommand)]
        command: Address,
    },
    Auth {
        #[command(subcommand)]
        command: Auth,
    },
    Mcp,
    Api {
        #[command(subcommand)]
        command: Option<ApiCommand>,
    },
    /// Print the generated API contract without loading an identity.
    #[command(hide = true)]
    Openapi,
}
#[derive(Args)]
struct List {
    #[arg(long)]
    address: Option<String>,
    #[arg(long)]
    from: Option<String>,
    #[arg(long)]
    subject: Option<String>,
    #[arg(long)]
    after_id: Option<i64>,
    #[arg(long, default_value_t = 20)]
    limit: u32,
}
impl From<List> for Filters {
    fn from(q: List) -> Self {
        Self {
            address: q.address,
            from_contains: q.from,
            subject_contains: q.subject,
            after_id: q.after_id,
            limit: Some(q.limit),
            ..Default::default()
        }
    }
}
#[derive(Subcommand)]
enum Inbox {
    List(List),
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
    List {
        #[arg(long, default_value_t = 0)]
        after_id: i64,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    Show {
        address_or_id: String,
    },
    Enable {
        address_or_id: String,
    },
    Disable {
        address_or_id: String,
    },
    Delete {
        address_or_id: String,
    },
}
#[derive(Subcommand)]
enum Auth {
    Fingerprint,
    Export {
        #[arg(long)]
        copy: bool,
    },
    Import {
        #[arg(conflicts_with = "stdin")]
        key: Option<String>,
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
fn color(text: &str, code: &str) -> String {
    if io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.into()
    }
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
    if matches!(command, Command::Version) {
        println!("ahrara {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if matches!(command, Command::Openapi) {
        print!("{}", ahrara::api::openapi_yaml()?);
        return Ok(());
    }
    let cfg = Config::load(cli.config.as_deref())?;
    if let Command::Auth {
        command: Auth::Import { key, stdin, yes },
    } = command
    {
        let _guard = Ownership::acquire(&cfg, "auth import", "")?;
        if cfg.identity_path.exists() {
            confirm(
                yes,
                "Replace the current Identity? Existing local email remains.",
            )?;
        }
        let secret = if let Some(key) = key {
            Zeroizing::new(key)
        } else if stdin || !io::stdin().is_terminal() {
            let mut s = Zeroizing::new(String::new());
            io::stdin().read_to_string(&mut s)?;
            s
        } else {
            Zeroizing::new(rpassword::prompt_password("Recovery key: ")?)
        };
        auth::Identity::import(&secret, &cfg)?;
        return output(
            &json!({"imported":true,"identity_fingerprint":auth::Identity::load(&cfg)?.fingerprint()}),
        );
    }
    if let Command::Auth {
        command: Auth::Regenerate { yes },
    } = command
    {
        let _guard = Ownership::acquire(&cfg, "auth regenerate", "")?;
        confirm(
            yes,
            "Generate a new Identity? Save the old Recovery Key first.",
        )?;
        let identity = auth::Identity::regenerate(&cfg)?;
        if io::stdout().is_terminal() && !cli.json {
            recovery(&identity);
        }
        return output(&json!({"regenerated":true,"identity_fingerprint":identity.fingerprint()}));
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
    let silent = matches!(command, Command::Mcp) || cli.json || !io::stdout().is_terminal();
    let (core, created) = Core::open(cfg)?;
    if created && !silent {
        recovery(&core.identity);
    }
    match command {
        Command::Listen => listen(core, cli.json).await,
        Command::Status => output(&core.status().await?),
        Command::Mcp => ahrara::mcp::serve(core).await,
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
                    ahrara::mail::terminal_safe(&e.date),
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
            Inbox::Clear => output(&json!({"deleted":core.storage.delete(None).await?})),
            Inbox::Delete { id } => {
                output(&json!({"deleted":core.storage.delete(Some(id)).await?}))
            }
            Inbox::Attachments { id } => output(&core.get_email(id, 1).await?.attachments),
            Inbox::Attachment {
                command:
                    Attachment::Save {
                        email_id,
                        attachment_id,
                        destination,
                    },
            } => output(
                &core
                    .save_attachment(email_id, attachment_id, &destination)
                    .await?,
            ),
        },
        _ => unreachable!(),
    }
}

async fn listen(core: Arc<Core>, json_output: bool) -> Result<()> {
    let mut events = core.events.subscribe();
    let status = core.start("listen", true).await?;
    if json_output || !io::stdout().is_terminal() {
        println!("{}", serde_json::to_string(&status)?);
    } else {
        println!("\nAhrara\n{}\n\nAddresses", color("Connected", "32"));
        for address in &status.addresses {
            println!("{}", address.address);
        }
        if status.addresses.is_empty() {
            println!(
                "{}",
                color("No enabled addresses. Enable or create one.", "33")
            );
        }
        println!("\nWaiting for email...\n");
    }
    let shutdown = ahrara::shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _=&mut shutdown=>break,
            event=events.recv()=>{
                let Ok(event)=event else{continue};
                if json_output || !io::stdout().is_terminal(){println!("{}",serde_json::to_string(&event)?);}
                else if event.kind=="email_received" {
                    let id=event.data["id"].as_i64().unwrap_or(0);
                    let e=core.get_email(id,1).await?;
                    println!("{}\n\nFrom        {}\nSubject     {}\nRecipient   {}\nReceived    {}\nSize        {} bytes\nAttachments {}\n\nSaved locally.\n",color("Email received","32"),ahrara::mail::terminal_safe(&e.metadata.from_header),ahrara::mail::terminal_safe(&e.metadata.subject),e.metadata.recipient,e.metadata.received_at,e.metadata.size_bytes,e.attachments.len());
                } else if event.kind=="connection_state_changed" {
                    let state=event.data["state"].as_str().unwrap_or("unknown");println!("{}",color(state,if state=="connected"{"32"}else{"33"}));
                }
            }
        }
    }
    core.stop().await;
    Ok(())
}
