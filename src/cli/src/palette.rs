use ahrara::{
    core::Core,
    web_state::{LAN_WARNING, Settings},
};
use anyhow::Result;
use crossterm::event::KeyCode;
use std::{process::Stdio, sync::Arc, time::Duration};
use zeroize::Zeroizing;

enum Screen {
    Root,
    Web,
    Mcp,
    Lan,
    Password {
        first: Zeroizing<String>,
        confirm: Zeroizing<String>,
        second: bool,
        enable_lan: bool,
    },
}

pub struct Palette {
    screen: Screen,
    selected: usize,
    settings: Settings,
    mcp_auth_required: bool,
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
            Screen::Root => vec!["Web Page".into(), "MCP".into()],
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
            Screen::Mcp => vec![format!(
                "{} MCP (Streamable HTTP)",
                if self.settings.mcp_enabled {
                    "Disable"
                } else {
                    "Enable"
                }
            )],
            Screen::Lan => vec![
                "Cancel".into(),
                "Set password".into(),
                "Continue without authentication".into(),
            ],
            Screen::Password { .. } => vec![],
        }
    }

    pub fn lines(&self, height: usize) -> Vec<(String, &'static str)> {
        let name = match &self.screen {
            Screen::Root => "Commands",
            Screen::Web => "Web Page",
            Screen::Mcp => "MCP",
            Screen::Lan => "LAN access",
            Screen::Password { .. } => "Web password",
        };
        let mut lines = vec![(format!("Ahrara / {name}"), "36;1"), (String::new(), "")];
        match &self.screen {
            Screen::Lan => lines.extend(LAN_WARNING.lines().map(|s| (s.to_owned(), "33"))),
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
        if !self.notice.is_empty() {
            lines.push((self.notice.clone(), "33"));
        }

        lines
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
            KeyCode::Enter => {
                self.notice.clear();
                match &self.screen {
                    Screen::Root => {
                        self.screen = match self.selected {
                            0 => Screen::Web,
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
                    Screen::Mcp => {
                        core.update_interfaces("mcp", !self.settings.mcp_enabled, false, None)
                            .await?;
                    }
                    Screen::Lan => match self.selected {
                        0 => self.screen = Screen::Web,
                        1 => self.password(true),
                        _ => {
                            core.update_interfaces("lan", true, true, None).await?;
                            self.screen = Screen::Web;
                        }
                    },
                    Screen::Password { .. } => {}
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
    async fn palette_requires_lan_choice_and_masks_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = Core::open(ahrara::config::Config {
            identity_path: dir.path().join("identity.key"),
            database_path: dir.path().join("inbox.db"),
            ..Default::default()
        })
        .unwrap();
        let mut palette = Palette::new(&core);

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
