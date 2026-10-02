use ahrara::{
    core::Core,
    storage::Address,
    web_state::{LAN_WARNING, Settings},
};
use anyhow::Result;
use crossterm::event::KeyCode;
use std::{process::Stdio, sync::Arc, time::Duration};
use zeroize::Zeroizing;

enum Screen {
    Root,
    Web,
    Emails,
    Mcp,
    Addresses,
    CopyAddress,
    Lan,
    Confirm(Destructive),
    Password {
        first: Zeroizing<String>,
        confirm: Zeroizing<String>,
        second: bool,
        enable_lan: bool,
    },
}
#[derive(Clone)]
enum Destructive {
    Address(String),
    Emails,
}
pub struct Palette {
    screen: Screen,
    selected: usize,
    settings: Settings,
    mcp_auth_required: bool,
    addresses: Vec<Address>,
    pages: Vec<i64>,
    pub notice: String,
    pub close: bool,
    pub copy: Option<String>,
    url: String,
}
impl Palette {
    pub fn new(core: &Core) -> Self {
        Self {
            screen: Screen::Root,
            selected: 0,
            settings: core.web.settings(),
            mcp_auth_required: core.mcp_auth_required(),
            addresses: vec![],
            pages: vec![0],
            notice: String::new(),
            close: false,
            copy: None,
            url: core.web_url(),
        }
    }
    fn password(&mut self, enable_lan: bool) {
        self.screen = Screen::Password {
            first: Zeroizing::new(String::new()),
            confirm: Zeroizing::new(String::new()),
            second: false,
            enable_lan,
        };
    }
    fn items(&self) -> Vec<String> {
        match &self.screen {
            Screen::Root => vec!["Web Page".into(), "Emails".into(), "MCP".into()],
            Screen::Web => vec![
                "Open Web Page".into(),
                format!(
                    "{} Web Page",
                    if self.settings.web_enabled {
                        "Disable"
                    } else {
                        "Enable"
                    }
                ),
                format!(
                    "{} LAN access",
                    if self.settings.lan_enabled {
                        "Disable"
                    } else {
                        "Enable"
                    }
                ),
                "Reset Password".into(),
            ],
            Screen::Emails => vec![
                "Delete address".into(),
                "Delete all emails".into(),
                "Clear attachments".into(),
                "Copy an email address".into(),
            ],
            Screen::Mcp => vec![format!(
                "{} MCP (Streamable HTTP)",
                if self.settings.mcp_enabled {
                    "Disable"
                } else {
                    "Enable"
                }
            )],
            Screen::Addresses | Screen::CopyAddress => self
                .addresses
                .iter()
                .map(|a| {
                    format!(
                        "{} — {}",
                        a.label.as_deref().unwrap_or("Unlabeled"),
                        a.address
                    )
                })
                .collect(),
            Screen::Lan => vec![
                "Cancel".into(),
                "Set password".into(),
                "Continue without authentication".into(),
            ],
            Screen::Confirm(_) => vec!["Cancel".into(), "Delete".into()],
            Screen::Password { .. } => vec![],
        }
    }
    pub fn lines(&self, height: usize) -> Vec<(String, &'static str)> {
        let name = match &self.screen {
            Screen::Root => "Commands",
            Screen::Web => "Web Page",
            Screen::Emails => "Emails",
            Screen::Mcp => "MCP",
            Screen::Addresses => "Delete address",
            Screen::CopyAddress => "Copy email address",
            Screen::Lan => "LAN access",
            Screen::Confirm(_) => "Confirm deletion",
            Screen::Password { .. } => "Web password",
        };
        let mut lines = vec![(format!("Ahrara / {name}"), "36;1"), (String::new(), "")];
        match &self.screen {
            Screen::Lan => lines.extend(LAN_WARNING.lines().map(|s| (s.to_owned(), "33"))),
            Screen::Confirm(Destructive::Address(id)) => lines.push((
                format!("Delete address #{id}? Existing emails will be kept."),
                "33",
            )),
            Screen::Confirm(Destructive::Emails) => lines.push((
                "Delete all local emails? This cannot be undone.".into(),
                "33",
            )),
            Screen::Password {
                first,
                confirm,
                second,
                ..
            } => {
                lines.push(("Use 8–1024 bytes. Esc cancels without saving.".into(), "2"));
                lines.push((
                    format!(
                        "{}: {}",
                        if *second {
                            "Confirm password"
                        } else {
                            "New password"
                        },
                        "*".repeat(
                            (if *second {
                                confirm.chars().count()
                            } else {
                                first.chars().count()
                            })
                            .min(60)
                        )
                    ),
                    "",
                ));
            }
            Screen::Web => {
                lines.push((self.url.clone(), ""));
                lines.push(("C Copy URL".into(), "2"));
            }
            Screen::Mcp => {
                lines.push((format!("{}/mcp", self.url), ""));
                lines.push((
                    format!(
                        "{}. C Copy URL",
                        if self.mcp_auth_required {
                            "Bearer token required"
                        } else {
                            "Authentication disabled"
                        }
                    ),
                    "2",
                ));
            }
            Screen::Emails => lines.push(("Deletes affect only local data.".into(), "2")),
            Screen::CopyAddress => lines.push(("Enter/C Copy address".into(), "2")),
            _ => {}
        }
        let capacity = height.saturating_sub(lines.len() + 4).max(1);
        let top = self.selected.saturating_sub(capacity - 1);
        lines.extend(
            self.items()
                .into_iter()
                .enumerate()
                .skip(top)
                .take(capacity)
                .map(|(i, item)| {
                    (
                        format!("{} {item}", if i == self.selected { "*" } else { " " }),
                        "",
                    )
                }),
        );
        if matches!(self.screen, Screen::Addresses | Screen::CopyAddress) {
            lines.push(("←/→ Address pages".into(), "2"));
        }
        if !self.notice.is_empty() {
            lines.push((self.notice.clone(), "33"));
        }
        lines
    }
    async fn load_addresses(&mut self, core: &Core) -> Result<()> {
        self.addresses = core
            .storage
            .addresses(
                core.identity.fingerprint(),
                *self.pages.last().unwrap(),
                20,
                false,
            )
            .await?;
        self.selected = 0;
        if self.addresses.is_empty() {
            self.notice = "No local addresses on this page.".into();
        }
        Ok(())
    }
    pub async fn input(&mut self, core: &Arc<Core>, key: KeyCode) -> Result<()> {
        if key == KeyCode::Esc {
            if matches!(self.screen, Screen::Root) {
                self.close = true;
            } else {
                self.screen = Screen::Root;
                self.selected = 0;
            }
            return Ok(());
        }
        if let Screen::Password {
            first,
            confirm,
            second,
            enable_lan,
        } = &mut self.screen
        {
            let buffer = if *second { &mut *confirm } else { &mut *first };
            match key {
                KeyCode::Char(c) if !c.is_control() && buffer.len() + c.len_utf8() <= 1024 => {
                    buffer.push(c)
                }
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Enter if !*second => *second = true,
                KeyCode::Enter => {
                    if **first != **confirm {
                        self.notice = "Passwords do not match. Esc cancels.".into();
                        return Ok(());
                    }
                    core.update_interfaces(
                        "password",
                        true,
                        false,
                        Some(Zeroizing::new(std::mem::take(&mut **first))),
                    )
                    .await?;
                    if *enable_lan {
                        core.update_interfaces("lan", true, false, None).await?;
                    }
                    self.screen = Screen::Web;
                    self.selected = 0;
                    self.notice =
                        "Web password saved. Existing browser sessions signed out.".into();
                }
                _ => {}
            }
            self.settings = core.web.settings();
            self.mcp_auth_required = core.mcp_auth_required();
            self.url = core.web_url();
            return Ok(());
        }
        if matches!(self.screen, Screen::CopyAddress)
            && matches!(key, KeyCode::Enter | KeyCode::Char('c' | 'C'))
        {
            self.copy = self.addresses.get(self.selected).map(|a| a.address.clone());
            return Ok(());
        }
        match key {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.items().len().saturating_sub(1))
            }
            KeyCode::Char('c' | 'C') if matches!(self.screen, Screen::Web) => {
                self.copy = Some(core.web_url())
            }
            KeyCode::Char('c' | 'C') if matches!(self.screen, Screen::Mcp) => {
                self.copy = Some(format!("{}/mcp", core.web_url()))
            }
            KeyCode::Right if matches!(self.screen, Screen::Addresses | Screen::CopyAddress) => {
                if let Some(last) = self.addresses.last() {
                    let next = core
                        .storage
                        .addresses(core.identity.fingerprint(), last.id, 1, false)
                        .await?;
                    if !next.is_empty() {
                        self.pages.push(last.id);
                        self.load_addresses(core).await?;
                    }
                }
            }
            KeyCode::Left
                if matches!(self.screen, Screen::Addresses | Screen::CopyAddress)
                    && self.pages.len() > 1 =>
            {
                self.pages.pop();
                self.load_addresses(core).await?;
            }
            KeyCode::Enter => {
                self.notice.clear();
                match &self.screen {
                    Screen::Root => {
                        self.screen = match self.selected {
                            0 => Screen::Web,
                            1 => Screen::Emails,
                            _ => Screen::Mcp,
                        }
                    }
                    Screen::Web => match self.selected {
                        0 => {
                            if !self.settings.web_enabled {
                                self.notice = "Enable the Web Page first.".into();
                            } else if !open_browser(&core.web_url()).await {
                                self.notice =
                                    "Browser unavailable. Press C to copy the URL.".into();
                            }
                        }
                        1 => {
                            core.update_interfaces("web", !self.settings.web_enabled, false, None)
                                .await?;
                        }
                        2 => {
                            if !self.settings.lan_enabled
                                && self.settings.password_hash.is_none()
                                && !self.settings.lan_no_auth_acknowledged
                            {
                                self.screen = Screen::Lan;
                            } else {
                                core.update_interfaces(
                                    "lan",
                                    !self.settings.lan_enabled,
                                    false,
                                    None,
                                )
                                .await?;
                            }
                        }
                        _ => self.password(false),
                    },
                    Screen::Emails => match self.selected {
                        0 => {
                            self.pages = vec![0];
                            self.load_addresses(core).await?;
                            self.screen = Screen::Addresses;
                        }
                        1 => self.screen = Screen::Confirm(Destructive::Emails),
                        2 => self.notice = "No cached attachments to clear.".into(),
                        _ => {
                            self.pages = vec![0];
                            self.load_addresses(core).await?;
                            self.screen = Screen::CopyAddress;
                        }
                    },
                    Screen::Mcp => {
                        core.update_interfaces("mcp", !self.settings.mcp_enabled, false, None)
                            .await?;
                    }
                    Screen::Addresses => {
                        if let Some(a) = self.addresses.get(self.selected) {
                            self.screen = Screen::Confirm(Destructive::Address(a.id.to_string()));
                        }
                    }
                    Screen::Confirm(action) => {
                        if self.selected == 1 {
                            match action.clone() {
                                Destructive::Address(id) => {
                                    core.change_address(id, None).await?;
                                }
                                Destructive::Emails => {
                                    core.delete_emails(None).await?;
                                }
                            }
                            self.notice = "Deleted.".into();
                        }
                        self.screen = Screen::Emails;
                    }
                    Screen::Lan => match self.selected {
                        0 => self.screen = Screen::Web,
                        1 => self.password(true),
                        _ => {
                            core.update_interfaces("lan", true, true, None).await?;
                            self.screen = Screen::Web;
                        }
                    },
                    Screen::Password { .. } | Screen::CopyAddress => {}
                }
                self.selected = 0;
            }
            _ => {}
        }
        self.settings = core.web.settings();
        self.mcp_auth_required = core.mcp_auth_required();
        self.url = core.web_url();
        Ok(())
    }
}
async fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = tokio::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = tokio::process::Command::new("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = tokio::process::Command::new("xdg-open");
    if std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some() {
        return false;
    }
    matches!(tokio::time::timeout(Duration::from_secs(2),command.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true).status()).await,Ok(Ok(status)) if status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn copy_address_picker_pages_without_deleting_addresses() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = Core::open(ahrara::config::Config {
            identity_path: dir.path().join("identity.key"),
            database_path: dir.path().join("inbox.db"),
            ..Default::default()
        })
        .unwrap();
        let mut addresses = vec![];
        for _ in 0..21 {
            addresses.push(core.create_address(None).await.unwrap());
        }
        let mut palette = Palette::new(&core);
        for key in [
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Down,
            KeyCode::Down,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Down,
            KeyCode::Char('c'),
        ] {
            palette.input(&core, key).await.unwrap();
        }
        assert_eq!(
            palette.copy.take().as_deref(),
            Some(addresses[1].address.as_str())
        );
        palette.input(&core, KeyCode::Right).await.unwrap();
        palette.input(&core, KeyCode::Enter).await.unwrap();
        assert_eq!(
            palette.copy.take().as_deref(),
            Some(addresses[20].address.as_str())
        );
        palette.input(&core, KeyCode::Left).await.unwrap();
        palette.input(&core, KeyCode::Enter).await.unwrap();
        assert_eq!(
            palette.copy.take().as_deref(),
            Some(addresses[0].address.as_str())
        );
        assert_eq!(
            core.storage
                .address_count(core.identity.fingerprint(), false)
                .await
                .unwrap(),
            21
        );
    }

    #[tokio::test]
    async fn palette_confirms_deletion_preserves_eml_and_masks_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = Core::open(ahrara::config::Config {
            identity_path: dir.path().join("identity.key"),
            database_path: dir.path().join("inbox.db"),
            ..Default::default()
        })
        .unwrap();
        let address = core.create_address(None).await.unwrap();
        let raw =
            b"From: Sender <sender@example.test>\r\nSubject: Palette test\r\n\r\nMessage.\r\n"
                .to_vec();
        core.storage
            .insert(
                "palette".into(),
                core.identity.fingerprint(),
                vec![address.address.clone()],
                "sender@example.test".into(),
                raw.clone(),
            )
            .await
            .unwrap();
        let mut palette = Palette::new(&core);
        // Emails -> Clear attachments leaves the original untouched.
        for key in [
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Down,
            KeyCode::Down,
            KeyCode::Enter,
        ] {
            palette.input(&core, key).await.unwrap();
        }
        assert_eq!(palette.notice, "No cached attachments to clear.");
        assert_eq!(core.storage.email(1).await.unwrap().1, raw);
        // Delete all requires selecting the affirmative option explicitly.
        for key in [KeyCode::Down, KeyCode::Enter, KeyCode::Enter] {
            palette.input(&core, key).await.unwrap();
        }
        assert!(core.storage.email(1).await.is_ok());
        for key in [KeyCode::Down, KeyCode::Enter, KeyCode::Down, KeyCode::Enter] {
            palette.input(&core, key).await.unwrap();
        }
        assert_eq!(core.storage.stats().await.unwrap().0, 0);
        assert!(
            core.storage
                .get_address(core.identity.fingerprint(), address.id.to_string())
                .await
                .is_ok()
        );
        palette = Palette::new(&core);
        for key in [KeyCode::Enter, KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
            palette.input(&core, key).await.unwrap();
        }
        assert!(matches!(palette.screen, Screen::Lan));
        palette.input(&core, KeyCode::Enter).await.unwrap();
        assert!(!core.web.settings().lan_enabled);
        palette.password(false);
        let password = ahrara::web_state::random_secret();
        for c in password.chars() {
            palette.input(&core, KeyCode::Char(c)).await.unwrap();
        }
        assert!(
            !palette
                .lines(24)
                .iter()
                .any(|(line, _)| line.contains(&password))
        );
        palette.input(&core, KeyCode::Esc).await.unwrap();
        assert!(core.web.settings().password_hash.is_none());
    }
}
