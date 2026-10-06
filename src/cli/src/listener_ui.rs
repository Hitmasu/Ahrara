use ahrara::{
    core::{ConnectionState, Core, Status},
    mail::{ParsedEmail, local_date, terminal_safe},
    storage::Email,
};
use anyhow::Result;
use console::{Term, measure_text_width, truncate_str};
use crossterm::{
    ExecutableCommand,
    clipboard::CopyToClipboard,
    cursor::MoveTo,
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use std::io::{self, IsTerminal, Write};

const LOGO: &str = r"    _    _   _ ____      _    ____      _
   / \  | | | |  _ \    / \  |  _ \    / \
  / _ \ | |_| | |_) |  / _ \ | |_) |  / _ \
 / ___ \|  _  |  _ <  / ___ \|  _ <  / ___ \
/_/   \_\_| |_|_| \_\/_/   \_\_| \_\/_/   \_\";
const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

#[derive(Default)]
pub struct Stats {
    web: Option<String>,
    mcp: Option<String>,
    lan_no_password: bool,
    default_address: Option<String>,
}

impl Stats {
    pub async fn refresh_address(&mut self, core: &Core) -> Result<()> {
        self.default_address = core
            .storage
            .latest_enabled_address(core.identity.fingerprint())
            .await?
            .map(|a| a.address);

        Ok(())
    }

    pub fn interfaces(&mut self, core: &Core) {
        let settings = core.web.settings();
        self.web = Some(if settings.web_enabled {
            core.web.status.lock().unwrap().clone()
        } else {
            "Disabled (/ to enable)".into()
        });
        self.mcp = Some(if settings.mcp_enabled {
            let http = core.web.status.lock().unwrap().clone();
            if http.starts_with("http://") || http.starts_with("https://") {
                format!("{http}/mcp")
            } else {
                http
            }
        } else {
            "Disabled (/ to enable)".into()
        });

        self.lan_no_password = settings.lan_enabled && settings.password_hash.is_none();
    }
}

// Only one metadata page and one opened message are kept in memory.
#[derive(Default)]
struct Browser {
    emails: Vec<Email>,
    selected: usize,
    top: usize,
    page_index: usize,
    new_mail: bool,
    reader: Option<Reader>,
    notice: String,
    palette: Option<crate::palette::Palette>,
}

impl Browser {
    async fn refresh(&mut self, core: &Core) -> Result<()> {
        self.emails = core
            .storage
            .email_page(core.identity.fingerprint(), None, false)
            .await?;
        self.selected = 0;
        self.top = 0;
        self.page_index = 0;
        self.new_mail = false;
        Ok(())
    }

    fn navigate(&mut self, down: bool) {
        self.selected = if down {
            (self.selected + 1).min(self.emails.len().saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
        };
    }

    async fn change_page(&mut self, core: &Core, older: bool) -> Result<()> {
        if !older && self.page_index == 0 {
            return Ok(());
        }

        let edge = if older {
            self.emails.last()
        } else {
            self.emails.first()
        };
        let Some(edge) = edge else {
            return Ok(());
        };
        let page = core
            .storage
            .email_page(core.identity.fingerprint(), Some(edge.id), !older)
            .await?;
        if !page.is_empty() {
            self.emails = page;
            self.selected = 0;
            self.top = 0;
            self.page_index = if older {
                self.page_index + 1
            } else {
                self.page_index - 1
            };
        }

        Ok(())
    }

    async fn open(&mut self, core: &Core) -> Result<()> {
        let Some(email) = self.emails.get(self.selected) else {
            return Ok(());
        };
        let email = core.get_email(email.id, 256 * 1024).await?;
        self.reader = Some(Reader::new(email));
        Ok(())
    }

    async fn toggle_raw(&mut self, core: &Core) -> Result<()> {
        let Some(reader) = &mut self.reader else {
            return Ok(());
        };
        if reader.raw {
            reader.show_body();
        } else {
            let (_, bytes) = core.storage.email(reader.email.metadata.id).await?;
            // Formatting a full 10 MiB EML must not block the async executor.
            reader.document =
                tokio::task::spawn_blocking(move || Document::new(display_source(&bytes))).await?;
            reader.raw = true;
        }

        Ok(())
    }
}

struct Reader {
    email: ParsedEmail,
    raw: bool,
    document: Document,
    viewport: usize,
}

impl Reader {
    fn new(email: ParsedEmail) -> Self {
        let mut reader = Self {
            email,
            raw: false,
            document: Document::new(String::new()),
            viewport: 1,
        };
        reader.show_body();
        reader
    }

    fn show_body(&mut self) {
        let mut body = display_source(self.email.text_body.as_bytes());
        if body.is_empty() {
            body = if self.email.parse_error {
                "Cannot parse this message. Press R to view the original EML."
            } else if self.email.html_available {
                "HTML-only message. Press R to view the original source."
            } else {
                "(Empty body)"
            }
            .into();
        }
        if self.email.text_body_truncated {
            body.push_str("\n\n[Body preview limited to 256 KiB. Press R for the complete EML.]");
        }

        self.document = Document::new(body);
        self.raw = false;
    }
}

struct Document {
    text: String,
    lines: Vec<std::ops::Range<usize>>,
    width: usize,
    scroll: usize,
}

impl Document {
    fn new(text: String) -> Self {
        Self {
            text,
            lines: Vec::new(),
            width: 0,
            scroll: 0,
        }
    }

    fn wrap(&mut self, width: usize) {
        if self.width == width {
            return;
        }

        self.width = width;
        self.lines.clear();
        let (mut start, mut columns) = (0, 0);
        let mut encoded = [0; 4];

        for (index, ch) in self.text.char_indices() {
            if ch == '\n' {
                self.lines.push(start..index);
                start = index + 1;
                columns = 0;
                continue;
            }

            let cells = measure_text_width(ch.encode_utf8(&mut encoded));
            if columns + cells > width && index > start {
                self.lines.push(start..index);
                start = index;
                columns = 0;
            }

            columns += cells;
        }

        self.lines.push(start..self.text.len());
    }
}

// Display every source byte without allowing message content to control the terminal.
// CRLF is rendered as a line break; other controls and invalid UTF-8 are escaped.
fn display_source(bytes: &[u8]) -> String {
    let mut output = String::new();
    for chunk in bytes.utf8_chunks() {
        let mut chars = chunk.valid().chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\r' if chars.peek() == Some(&'\n') => {}
                '\n' => output.push('\n'),
                '\t' => output.push_str("    "),
                ch if ch.is_control() => output.push_str(&format!("\\u{{{:x}}}", ch as u32)),
                ch => output.push(ch),
            }
        }

        for byte in chunk.invalid() {
            output.push_str(&format!("\\x{byte:02x}"));
        }
    }

    output
}

pub struct ListenerUi {
    terminal: Term,
    colors: bool,
    previous: String,
    size: (u16, u16),
    raw_input: bool,
    alternate_screen: bool,
    clipboard: Option<arboard::Clipboard>,
    browser: Browser,
}

impl ListenerUi {
    pub fn new() -> io::Result<Self> {
        let mut ui = Self {
            terminal: Term::buffered_stdout(),
            colors: std::env::var_os("NO_COLOR").is_none() && console::colors_enabled(),
            previous: String::new(),
            size: (0, 0),
            raw_input: false,
            alternate_screen: false,
            clipboard: None,
            browser: Browser::default(),
        };
        if io::stdin().is_terminal() {
            enable_raw_mode()?;
            ui.raw_input = true;
            ui.terminal.execute(EnableBracketedPaste)?;
        }

        ui.terminal.execute(EnterAlternateScreen)?;
        ui.alternate_screen = true;
        ui.terminal.hide_cursor()?;
        Ok(ui)
    }

    pub async fn refresh(&mut self, core: &Core) -> Result<()> {
        self.browser.refresh(core).await
    }

    pub fn input_stream(&self) -> Option<EventStream> {
        self.raw_input.then(EventStream::new)
    }

    pub async fn handle_input(
        &mut self,
        core: &std::sync::Arc<Core>,
        stats: &Stats,
        event: Event,
    ) -> Result<bool> {
        let Event::Key(key) = event else {
            return Ok(false);
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            return Ok(true);
        }
        if !key.modifiers.is_empty() && key.modifiers != KeyModifiers::SHIFT {
            return Ok(false);
        }
        if let Some(palette) = &mut self.browser.palette {
            if let Err(error) = palette.input(core, key.code).await {
                palette.notice = error.to_string();
            }

            let copy = palette.copy.take();
            let close = palette.close;
            if let Some(value) = copy {
                self.copy_text(&value)?;
            }
            if close {
                self.browser.palette = None;
                self.refresh(core).await?;
            }

            return Ok(false);
        }

        self.browser.notice.clear();
        let result = match key.code {
            KeyCode::Char('/') => {
                self.browser.palette = Some(crate::palette::Palette::new(core));
                Ok(())
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
                if self.browser.reader.is_some() =>
            {
                let reader = self.browser.reader.as_mut().unwrap();
                let step = if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                    reader.viewport
                } else {
                    1
                };
                if matches!(key.code, KeyCode::Up | KeyCode::PageUp) {
                    reader.document.scroll = reader.document.scroll.saturating_sub(step);
                } else {
                    reader.document.scroll = reader.document.scroll.saturating_add(step);
                }

                Ok(())
            }
            KeyCode::Up | KeyCode::Down => {
                self.browser.navigate(key.code == KeyCode::Down);
                Ok(())
            }
            KeyCode::Left | KeyCode::Right if self.browser.reader.is_none() => {
                self.browser
                    .change_page(core, key.code == KeyCode::Right)
                    .await
            }
            KeyCode::Enter if self.browser.reader.is_none() => self.browser.open(core).await,
            KeyCode::Char('r' | 'R') => self.browser.toggle_raw(core).await,
            KeyCode::Esc => {
                if self.browser.reader.as_ref().is_some_and(|r| r.raw) {
                    self.browser.reader.as_mut().unwrap().show_body();
                } else {
                    self.browser.reader = None;
                }

                Ok(())
            }
            KeyCode::Home => {
                if let Some(reader) = &mut self.browser.reader {
                    reader.document.scroll = 0;
                    Ok(())
                } else {
                    self.refresh(core).await
                }
            }
            KeyCode::End => {
                if let Some(reader) = &mut self.browser.reader {
                    reader.document.scroll = usize::MAX;
                }

                Ok(())
            }
            KeyCode::Char('c' | 'C') => self
                .copy_address(stats.default_address.as_deref())
                .map_err(Into::into),
            _ => Ok(()),
        };
        if let Err(error) = result {
            // A concurrently deleted email must not stop the listener.
            self.browser.notice = format!("Unable to open or browse email: {error}");
        }

        Ok(false)
    }

    fn copy_address(&mut self, address: Option<&str>) -> io::Result<()> {
        let Some(address) = address else {
            self.browser.notice = "No enabled address to copy".into();
            return Ok(());
        };

        self.copy_text(address)
    }

    fn copy_text(&mut self, value: &str) -> io::Result<()> {
        let remote =
            std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some();
        if !remote {
            if self.clipboard.is_none() {
                self.clipboard = arboard::Clipboard::new().ok();
            }

            if self
                .clipboard
                .as_mut()
                .is_some_and(|clipboard| clipboard.set_text(value).is_ok())
            {
                self.browser.notice = "Address copied".into();
                return Ok(());
            }
        }

        self.terminal
            .execute(CopyToClipboard::to_clipboard_from(value))?;
        self.browser.notice = "Copy sent to terminal".into();
        Ok(())
    }

    pub async fn inbox_deleted(&mut self, core: &Core, id: Option<i64>) -> Result<()> {
        if self
            .browser
            .reader
            .as_ref()
            .is_some_and(|reader| id.is_none_or(|id| reader.email.metadata.id == id))
        {
            self.browser.reader = None;
        }

        self.refresh(core).await?;
        self.browser.notice = if id.is_some() {
            "Email deleted"
        } else {
            "Inbox cleared"
        }
        .into();

        Ok(())
    }

    pub async fn email_received(&mut self, core: &Core) -> Result<()> {
        if self.browser.reader.is_none()
            && self.browser.page_index == 0
            && self.browser.selected == 0
        {
            self.refresh(core).await?;
        } else {
            self.browser.new_mail = true;
        }

        Ok(())
    }

    pub fn update_notice(&mut self, message: String) {
        self.browser.notice = message;
    }

    pub fn draw(
        &mut self,
        state: &ConnectionState,
        status: Option<&Status>,
        stats: &Stats,
        tick: usize,
    ) -> io::Result<()> {
        let size = self.terminal.size();
        let frame = render(
            state,
            status,
            &mut self.browser,
            stats,
            tick,
            size,
            self.colors,
        );
        if frame == self.previous && size == self.size {
            return Ok(());
        }

        self.terminal.execute(MoveTo(0, 0))?;
        self.terminal.execute(Clear(ClearType::All))?;
        self.terminal
            .write_all(frame.replace('\n', "\r\n").as_bytes())?;
        self.terminal.flush()?;
        self.previous = frame;
        self.size = size;
        Ok(())
    }
}

impl Drop for ListenerUi {
    fn drop(&mut self) {
        if self.raw_input {
            let _ = self.terminal.execute(DisableBracketedPaste);
            let _ = disable_raw_mode();
        }
        if self.alternate_screen {
            let _ = self.terminal.execute(LeaveAlternateScreen);
        }

        let _ = self.terminal.show_cursor();
        let _ = self.terminal.flush();
    }
}

fn render(
    state: &ConnectionState,
    status: Option<&Status>,
    browser: &mut Browser,
    stats: &Stats,
    tick: usize,
    size: (u16, u16),
    colors: bool,
) -> String {
    let width = usize::from(size.1.saturating_sub(1)).min(120);
    let height = usize::from(size.0.saturating_sub(1));
    if width == 0 || height == 0 {
        return String::new();
    }

    let footer_height = if browser.notice.is_empty() { 1 } else { 2 };
    let mut lines = if let Some(palette) = &browser.palette {
        palette.lines(height)
    } else if let Some(reader) = &mut browser.reader {
        reader_lines(reader, width, height)
    } else {
        let title_height = usize::from(height >= 15);
        let rows = browser.emails.len();
        let dashboard_height = height.saturating_sub(footer_height + 4 + rows + title_height);
        let mut lines = dashboard(state, status, stats, tick, width, height);
        if lines.len() > dashboard_height {
            lines = dashboard(state, status, stats, tick, width, 19);
        }
        if lines.len() > dashboard_height {
            lines = dashboard(state, status, stats, tick, width, 9);
        }

        lines.truncate(dashboard_height);
        if title_height > 0 {
            lines.push((
                format!(
                    "Inbox — Page {} — {}",
                    browser.page_index + 1,
                    if browser.new_mail {
                        "new mail: Home to refresh"
                    } else {
                        "newest first"
                    }
                ),
                "36;1",
            ));
        }

        lines.push((email_border(width, ['┌', '┬', '┐']), "2"));
        lines.push((
            email_columns(
                [
                    if width >= 72 {
                        "Received (local)"
                    } else {
                        "Time"
                    },
                    "From",
                    "Subject",
                    "Size",
                ],
                width,
                false,
            ),
            "1",
        ));
        if !browser.emails.is_empty() {
            lines.push((email_border(width, ['├', '┼', '┤']), "2"));
        }

        let capacity = height
            .saturating_sub(lines.len() + footer_height + 1)
            .max(1);
        if browser.selected < browser.top {
            browser.top = browser.selected;
        }
        if browser.selected >= browser.top + capacity {
            browser.top = browser.selected + 1 - capacity;
        }

        for (index, email) in browser
            .emails
            .iter()
            .enumerate()
            .skip(browser.top)
            .take(capacity)
        {
            lines.push((email_line(email, width, index == browser.selected), ""));
        }

        lines.push((email_border(width, ['└', '┴', '┘']), "2"));
        if browser.emails.is_empty() {
            lines.push((
                format!(
                    "  {} Waiting for email...",
                    if state.state == "connected" {
                        SPINNER[tick % SPINNER.len()]
                    } else {
                        "·"
                    }
                ),
                "33",
            ));
        }

        lines
    };
    let footer = if browser.palette.is_some() {
        "↑/↓ Select  Enter Confirm  Esc Back  Ctrl+C Stop"
    } else if browser.reader.is_some() {
        "↑/↓ Scroll  PgUp/PgDn  Home/End  R Body/raw  Esc Back  Ctrl+C Stop"
    } else {
        "↑/↓ Select  ←/→ Page  Enter Open  / Menu  C Copy  Ctrl+C Stop"
    };
    lines.truncate(height.saturating_sub(footer_height));

    while lines.len() < height.saturating_sub(footer_height) {
        lines.push((String::new(), ""));
    }
    if footer_height == 2 && height >= 2 {
        lines.push((browser.notice.clone(), "33;1"));
    }

    lines.push((footer.into(), "2"));
    let mut output = String::new();

    for (text, color) in lines.into_iter().take(height) {
        let safe = terminal_safe(&text).replace(['\n', '\t'], " ");
        let clipped = truncate_str(&safe, width, if width >= 3 { "..." } else { "" });
        if colors && !color.is_empty() {
            output.push_str(&format!("\x1b[{color}m{clipped}\x1b[0m\n"));
        } else {
            output.push_str(&clipped);
            output.push('\n');
        }
    }

    output
}

fn reader_lines(reader: &mut Reader, width: usize, height: usize) -> Vec<(String, &'static str)> {
    let e = &reader.email;
    let m = &e.metadata;
    let mut lines = if reader.raw {
        vec![
            (format!("Raw EML / #{}", m.id), "36;1"),
            ("Complete source; terminal controls are escaped".into(), "2"),
        ]
    } else {
        vec![
            (format!("Message / #{}", m.id), "36;1"),
            (
                format!(
                    "From     {}",
                    if m.from_header.is_empty() {
                        &m.envelope_sender
                    } else {
                        &m.from_header
                    }
                ),
                "",
            ),
            (format!("To       {}", m.to_header), ""),
            (
                format!(
                    "Cc       {}",
                    if e.cc.is_empty() { "(none)" } else { &e.cc }
                ),
                "",
            ),
            (
                format!(
                    "Subject  {}",
                    if m.subject.is_empty() {
                        "(no subject)"
                    } else {
                        &m.subject
                    }
                ),
                "1",
            ),
            (
                format!(
                    "Date     {}",
                    local_date(
                        if e.date.is_empty() {
                            &m.received_at
                        } else {
                            &e.date
                        },
                        "%Y-%m-%d %H:%M:%S %:z"
                    )
                ),
                "",
            ),
            (format!("Recipient {}", m.recipient), "2"),
        ]
    };
    lines.truncate(height.saturating_sub(4));
    lines.push(("─".repeat(width), "2"));
    reader.viewport = height.saturating_sub(lines.len() + 2).max(1);
    reader.document.wrap(width);
    reader.document.scroll = reader
        .document
        .scroll
        .min(reader.document.lines.len().saturating_sub(reader.viewport));
    lines.extend(
        reader
            .document
            .lines
            .iter()
            .skip(reader.document.scroll)
            .take(reader.viewport)
            .map(|range| (reader.document.text[range.clone()].to_owned(), "")),
    );

    lines
}

fn dashboard(
    state: &ConnectionState,
    status: Option<&Status>,
    stats: &Stats,
    tick: usize,
    width: usize,
    height: usize,
) -> Vec<(String, &'static str)> {
    let spinner = SPINNER[tick % SPINNER.len()];
    let (indicator, label, color) = match state.state.as_str() {
        "connected" => ("+", "Connected", "32;1"),
        "reconnecting" => (spinner, "Reconnecting...", "33;1"),
        "failed" => ("!", "Connection failed", "31;1"),
        "stopped" => ("-", "Stopped", "2"),
        _ => (spinner, "Connecting...", "36;1"),
    };
    let address = match status {
        Some(status) => match &stats.default_address {
            Some(address) if status.enabled_addresses_count > 1 => {
                format!("{} (+{} more)", address, status.enabled_addresses_count - 1)
            }
            Some(address) => address.clone(),
            None => "None enabled".into(),
        },
        None => "Preparing local address...".into(),
    };
    let mut fields = vec![
        ("● Status", format!("[{indicator}] {label}"), color),
        ("✉ Email", address, ""),
    ];
    if let Some(web) = &stats.web {
        fields.push((
            "◉ Web",
            format!(
                "{}{}",
                web,
                if stats.lan_no_password {
                    " · LAN · No password"
                } else {
                    ""
                }
            ),
            if stats.lan_no_password { "33" } else { "" },
        ));
    }
    if let Some(mcp) = &stats.mcp {
        fields.push(("◇ MCP", mcp.clone(), ""));
    }

    let mut lines = Vec::<(String, &str)>::new();
    if width >= 26 && height >= 13 {
        let key_width = 12;
        let value_width = width - key_width - 7;
        lines.push((format!("┌{}┐", "─".repeat(width - 2)), "2"));
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        let logo_space = (width - 4).saturating_sub(version.len() + 2);
        let logo_width = LOGO.lines().map(measure_text_width).max().unwrap_or(0);
        if logo_space >= logo_width && height >= 20 {
            let indent = " ".repeat((logo_space - logo_width) / 2);
            for (index, line) in LOGO.lines().enumerate() {
                let text = format!("{indent}{line}");
                let header = if index == 0 {
                    format!("{}  {version}", cell(&text, logo_space))
                } else {
                    cell(&text, width - 4)
                };

                lines.push((format!("│ {header} │"), "36;1"));
            }
        } else {
            lines.push((
                format!("│ {}  {version} │", cell("AHRARA", logo_space)),
                "36;1",
            ));
        }

        lines.push((
            format!(
                "├{}┬{}┤",
                "─".repeat(key_width + 2),
                "─".repeat(value_width + 2)
            ),
            "2",
        ));

        for (key, value, color) in fields {
            lines.push((
                format!(
                    "│ {} │ {} │",
                    cell(key, key_width),
                    cell(&value, value_width)
                ),
                color,
            ));
        }

        lines.push((
            format!(
                "└{}┴{}┘",
                "─".repeat(key_width + 2),
                "─".repeat(value_width + 2)
            ),
            "2",
        ));
    } else {
        lines.push((format!("AHRARA   v{}", env!("CARGO_PKG_VERSION")), "36;1"));
        for (key, value, color) in fields {
            lines.push((format!("{key}: {value}"), color));
        }
    }

    lines
}

// Column indices refer to selection, received time, sender, subject and size.
// Compact terminals keep selection, subject and size.
fn email_layout(width: usize) -> Vec<(usize, usize)> {
    if width >= 60 {
        let date = if width >= 72 { 19 } else { 8 };
        let flexible = width - 16 - 1 - date - 8;
        let sender = (flexible * 2 / 5).max(12);
        vec![
            (0, 1),
            (1, date),
            (2, sender),
            (3, flexible - sender),
            (4, 8),
        ]
    } else if width >= 24 {
        vec![(0, 1), (3, width - 19), (4, 8)]
    } else {
        vec![(0, 1), (3, width.saturating_sub(8))]
    }
}

fn email_border(width: usize, [left, middle, right]: [char; 3]) -> String {
    let cells = email_layout(width)
        .into_iter()
        .map(|(_, width)| "─".repeat(width + 2))
        .collect::<Vec<_>>()
        .join(&middle.to_string());

    format!("{left}{cells}{right}")
}

fn email_columns(values: [&str; 4], width: usize, selected: bool) -> String {
    let values = [
        if selected { "*" } else { "" },
        values[0],
        values[1],
        values[2],
        values[3],
    ];
    let cells = email_layout(width)
        .into_iter()
        .map(|(index, width)| cell(values[index], width))
        .collect::<Vec<_>>()
        .join(" │ ");

    format!("│ {cells} │")
}

fn email_line(email: &Email, width: usize, selected: bool) -> String {
    let date = local_date(
        &email.received_at,
        if width >= 72 {
            "%Y-%m-%d %H:%M:%S"
        } else {
            "%H:%M:%S"
        },
    );
    let sender = if email.from_header.is_empty() {
        &email.envelope_sender
    } else {
        &email.from_header
    };
    let subject = if email.subject.is_empty() {
        "(no subject)"
    } else {
        &email.subject
    };

    email_columns(
        [
            &date,
            sender,
            subject,
            &ahrara::web::bytes(email.size_bytes.max(0) as usize),
        ],
        width,
        selected,
    )
}

fn cell(text: &str, width: usize) -> String {
    let safe = terminal_safe(text).replace(['\n', '\t'], " ");
    let clipped = truncate_str(&safe, width, if width >= 3 { "..." } else { "" });
    format!(
        "{clipped}{}",
        " ".repeat(width - measure_text_width(&clipped))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_inbox_waits_below_the_table_in_yellow() {
        let state = ConnectionState {
            state: "connected".into(),
            ..Default::default()
        };
        let frame = render(
            &state,
            None,
            &mut Browser::default(),
            &Stats::default(),
            0,
            (50, 80),
            false,
        );
        let table = frame
            .lines()
            .skip_while(|line| !line.starts_with("Inbox"))
            .skip(1)
            .take_while(|line| !line.starts_with('└'))
            .collect::<Vec<_>>();

        assert_eq!(
            table.len(),
            2,
            "empty table only has its top and heading before the bottom border"
        );
        let lines: Vec<_> = frame.lines().collect();
        let waiting = lines
            .iter()
            .position(|line| line.contains("Waiting for email"))
            .unwrap();

        assert!(lines[waiting - 1].starts_with('└'));
        assert!(!lines[waiting].contains('│'));
        assert!(!frame.contains('\x1b'));
        let colored = render(
            &state,
            None,
            &mut Browser::default(),
            &Stats::default(),
            0,
            (50, 80),
            true,
        );

        assert!(
            colored
                .lines()
                .any(|line| line.starts_with("\x1b[33m") && line.contains("Waiting for email"))
        );
    }

    #[test]
    fn spinner_animates_connecting_and_empty_inbox_without_adding_colors() {
        for state in [
            "connecting",
            "reconnecting",
            "connected",
            "failed",
            "stopped",
        ] {
            let connection = ConnectionState {
                state: state.into(),
                ..Default::default()
            };
            let stats = Stats::default();
            let first = render(
                &connection,
                None,
                &mut Browser::default(),
                &stats,
                0,
                (24, 80),
                false,
            );
            let next = render(
                &connection,
                None,
                &mut Browser::default(),
                &stats,
                1,
                (24, 80),
                false,
            );

            assert!(!first.contains('\x1b'));
            assert!(first.contains(LOGO.lines().next().unwrap()));
            assert_eq!(
                first != next,
                matches!(state, "connecting" | "reconnecting" | "connected")
            );
        }
    }

    #[test]
    fn narrow_panels_fit_and_untrusted_email_cannot_add_terminal_commands_or_rows() {
        let state = ConnectionState {
            state: "connected".into(),
            ..Default::default()
        };
        let mut email = Email {
            id: 1,
            transfer_id: "test".into(),
            identity_fingerprint: "test".into(),
            recipient: "receiver@ahrara.dev".into(),
            envelope_sender: "sender@example.test".into(),
            received_at: "2026-09-26T12:00:00Z".into(),
            from_header: "Sender".into(),
            to_header: String::new(),
            subject: String::new(),
            message_id: None,
            size_bytes: 4,
        };
        email.subject = "日本語\n\t\x1b[31mSubject".repeat(50);
        let stats = Stats::default();

        for size in [
            (24u16, 80u16),
            (32, 120),
            (32, 160),
            (12, 32),
            (6, 20),
            (2, 2),
            (0, 0),
        ] {
            let mut browser = Browser {
                emails: vec![email.clone()],
                ..Default::default()
            };
            let frame = render(&state, None, &mut browser, &stats, 0, size, false);
            if size == (12, 32) {
                assert!(frame.contains("✉ Email"));
                assert!(frame.contains("v0.1.0"));
            }

            assert!(frame.lines().count() <= usize::from(size.0.saturating_sub(1)));
            assert!(
                frame
                    .lines()
                    .all(|line| measure_text_width(line) < usize::from(size.1))
            );

            assert!(!frame.contains('\x1b') && !frame.contains('\t'));
        }

        let mut browser = Browser {
            emails: vec![email.clone()],
            ..Default::default()
        };
        email.subject = "Second message".into();
        browser.emails.push(email);
        let frame = render(&state, None, &mut browser, &stats, 0, (32, 80), false);

        assert!(frame.contains("日本語") && frame.contains("Second message"));
        let local_received = local_date("2026-09-26T12:00:00Z", "%Y-%m-%d %H:%M:%S");

        assert!(frame.contains(&local_received) && frame.contains("4 B"));
        assert!(!frame.contains("Received this session"));
        let table = frame
            .split("Inbox — Page 1 — newest first\n")
            .nth(1)
            .unwrap();
        let table_lines: Vec<_> = table.lines().collect();

        assert!(table_lines[0].starts_with('┌') && table_lines[0].contains('┬'));
        assert!(table_lines[1].starts_with('│') && table_lines[1].ends_with('│'));
        assert_eq!(table_lines[1].matches('│').count(), 6);
        assert!(table_lines[2].starts_with('├') && table_lines[2].contains('┼'));
        assert!(
            table_lines
                .iter()
                .any(|line| line.starts_with('└') && line.contains('┴'))
        );
        assert!(
            table_lines
                .iter()
                .filter(|line| line.starts_with('│'))
                .all(|line| measure_text_width(line) == 79)
        );
        assert!(
            frame
                .lines()
                .any(|line| line.starts_with(&format!("│ * │ {local_received} │")))
        );
        let sample = browser.emails[0].clone();

        for count in [1, 3, 10] {
            for size in [(24, 80), (50, 80)] {
                let mut page = Browser {
                    emails: vec![sample.clone(); count],
                    ..Default::default()
                };
                let frame = render(&state, None, &mut page, &stats, 0, size, false);
                let table = frame
                    .lines()
                    .skip_while(|line| !line.starts_with("Inbox"))
                    .skip(1)
                    .take_while(|line| !line.starts_with('└'))
                    .collect::<Vec<_>>();

                assert_eq!(
                    table.len(),
                    count + 3,
                    "only actual rows plus top, heading and separator"
                );
                let next = render(&state, None, &mut page, &stats, 1, size, false);

                assert_eq!(frame, next, "waiting animation stops once mail exists");
            }
        }
    }

    #[test]
    fn table_aligns_values() {
        let frame = render(
            &ConnectionState::default(),
            None,
            &mut Browser::default(),
            &Stats::default(),
            0,
            (24, 80),
            false,
        );

        for line in frame.lines().take_while(|line| !line.starts_with("Inbox")) {
            assert_eq!(measure_text_width(line), 79);
        }

        assert_eq!(measure_text_width(&cell("日本語\t\x1b[31m", 12)), 12);
    }

    #[tokio::test]
    async fn inbox_pages_selection_and_reader_use_local_core() {
        let dir = tempfile::tempdir().unwrap();
        let (core, _) = Core::open(ahrara::config::Config {
            database_path: dir.path().join("inbox.db"),
            identity_path: dir.path().join("identity.key"),
            api_token_path: dir.path().join("api.token"),
            ..Default::default()
        })
        .unwrap();
        let address = core.create_address(None).await.unwrap().address;
        let fingerprint = core.identity.fingerprint();
        let mut ids = Vec::new();

        for index in 0..23 {
            let rows = core.storage.insert(index.to_string(), fingerprint.clone(), vec![address.clone()], "sender@example.test".into(),
                format!("From: Sender <sender@example.test>\r\nTo: {address}\r\nCc: copy@example.test\r\nSubject: Message {index}\r\n\r\nBody {index}\r\n").into_bytes()).await.unwrap();
            ids.push(rows[0].id);
        }

        // A previous Identity's history must not appear in the active inbox.
        core.storage
            .insert(
                "other".into(),
                "other-identity".into(),
                vec![address],
                "sender".into(),
                b"\r\nOther identity".to_vec(),
            )
            .await
            .unwrap();
        let mut browser = Browser::default();
        browser.refresh(&core).await.unwrap();

        assert_eq!(browser.emails.len(), 10);
        assert_eq!(browser.emails[0].id, ids[22]);
        browser.change_page(&core, false).await.unwrap();

        assert_eq!(browser.page_index, 0);

        for _ in 0..100 {
            browser.navigate(true);
        }

        assert_eq!(browser.emails[browser.selected].id, ids[13]);
        assert_eq!(browser.page_index, 0, "down stays within the current page");
        browser.navigate(false);

        assert_eq!(browser.emails[browser.selected].id, ids[14]);
        let mut seen = browser.emails.iter().map(|e| e.id).collect::<Vec<_>>();
        browser.change_page(&core, true).await.unwrap();

        assert_eq!(browser.page_index, 1);
        assert_eq!(browser.emails.len(), 10);
        assert_eq!(browser.emails[0].id, ids[12]);
        assert_eq!(browser.selected, 0);
        seen.extend(browser.emails.iter().map(|e| e.id));
        browser.change_page(&core, true).await.unwrap();

        assert_eq!(browser.page_index, 2);
        assert_eq!(browser.emails.len(), 3);
        seen.extend(browser.emails.iter().map(|e| e.id));

        assert_eq!(seen, ids.iter().rev().copied().collect::<Vec<_>>());
        browser.change_page(&core, true).await.unwrap();

        assert_eq!(browser.page_index, 2, "right at the last page is a no-op");
        browser.change_page(&core, false).await.unwrap();

        assert_eq!(browser.page_index, 1);
        assert_eq!(browser.emails[0].id, ids[12]);
        browser.open(&core).await.unwrap();
        let reader = browser.reader.as_ref().unwrap();

        assert_eq!(reader.email.cc, "copy@example.test");
        assert!(reader.document.text.contains("Body 12"));
        browser.toggle_raw(&core).await.unwrap();
        let reader = browser.reader.as_ref().unwrap();

        assert!(reader.raw && reader.document.text.contains("Cc: copy@example.test"));
        browser.toggle_raw(&core).await.unwrap();

        assert!(!browser.reader.as_ref().unwrap().raw);
        let id = browser.emails[browser.selected].id;
        core.storage.delete(Some(id)).await.unwrap();
        browser.reader = None;

        assert!(browser.open(&core).await.is_err());
        browser.refresh(&core).await.unwrap();

        assert_eq!(browser.emails[0].id, ids[22]);
    }

    #[test]
    fn source_is_complete_safe_and_wraps_without_losing_content() {
        let mut raw = b"Subject: source\r\n\r\n\x1b[31m\xff\t".to_vec();
        raw.extend("日本語 long line ".repeat(24_000).as_bytes());
        raw.extend(b"\r\nEND OF ORIGINAL");
        let source = display_source(&raw);

        assert!(source.len() > 256 * 1024);
        assert!(source.ends_with("END OF ORIGINAL"));
        assert!(source.contains(r"\u{1b}[31m\xff"));
        assert!(!source.contains('\x1b') && !source.contains('\r') && !source.contains('\t'));
        let mut document = Document::new(source);

        for width in [76, 30, 8] {
            document.wrap(width);
            assert!(
                document
                    .lines
                    .iter()
                    .all(|range| measure_text_width(&document.text[range.clone()]) <= width)
            );
            let visible = document
                .lines
                .iter()
                .map(|range| &document.text[range.clone()])
                .collect::<String>();

            assert_eq!(visible, document.text.replace('\n', ""));
        }
    }
}
