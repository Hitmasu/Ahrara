//! Prometheus metrics and the `rule_applied` log. Labels are fixed words: no
//! metric or log line carries an address, route, Identity or message detail.
//! Only `rule_applied` names the IP address a protection rule was applied to.
use crate::broker::Broker;
use axum::{Router, extract::State, http::header::CONTENT_TYPE, routing::get};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write,
    net::IpAddr,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub static CLIENT_SESSIONS: AtomicU64 = AtomicU64::new(0);
pub static EMAIL_BYTES: AtomicU64 = AtomicU64::new(0);
pub static WEBSOCKET_IN: AtomicU64 = AtomicU64::new(0);
pub static WEBSOCKET_OUT: AtomicU64 = AtomicU64::new(0);

const EMAILS: &str = "ahrara_emails_total";
const REFUSED: &str = "ahrara_recipients_refused_total";
const RULES: &str = "ahrara_rule_hits_total";

/// Every word each labelled counter takes. All are exported from the start,
/// because Prometheus' increase() misses a series whose first sample is 1.
const WORDS: [(&str, &[&str]); 3] = [
    (
        EMAILS,
        &[
            "delivered",
            "recipient_refused",
            "too_large",
            "expired",
            "ack_timeout",
            "nack",
            "client_disconnected",
            "sender_error",
            "forwarding_invalid",
        ],
    ),
    (
        REFUSED,
        &[
            "offline",
            "invalid_address",
            "cooldown",
            "busy",
            "rejected",
            "too_many",
            "retried",
            "reconnected",
            "offer_unavailable",
        ],
    ),
    (
        RULES,
        &[
            "origin_not_allowed",
            "clients_limit",
            "clients_per_source_limit",
            "handshake_timeout",
            "invalid_hello",
            "invalid_public_key",
            "authentication_failed",
            "proof_of_work_failed",
            "identity_in_use",
            "protocol_violation",
            "smtp_routing_only",
            "smtp_sessions_limit",
            "smtp_sessions_per_source_limit",
            "smtp_starttls_required",
            "smtp_too_large",
            "smtp_syntax_errors",
            "smtp_no_progress",
            "delivery_cooldown",
        ],
    ),
];

/// Counters split by one fixed word, keyed by (metric, word).
static LABELLED: Mutex<BTreeMap<(&str, &str), u64>> = Mutex::new(BTreeMap::new());

pub fn add(counter: &AtomicU64, value: usize) {
    counter.fetch_add(value as u64, Relaxed);
}

fn count(metric: &'static str, word: &'static str) {
    debug_assert!(
        WORDS
            .iter()
            .any(|(name, words)| *name == metric && words.contains(&word)),
        "{word} is not listed for {metric}"
    );
    *LABELLED.lock().unwrap().entry((metric, word)).or_default() += 1;
}

/// The outcome of one message that reached DATA.
pub fn email(result: &'static str) {
    count(EMAILS, result);
}

pub fn recipient_refused(reason: &'static str) {
    count(REFUSED, reason);
}

const RULE_LOG_WINDOW: Duration = Duration::from_secs(60);
const RULE_LOG_KEYS: usize = 4096;

type RuleKey = (&'static str, Option<IpAddr>);

/// When each (rule, address) was last logged and how often it applied since.
static RULE_LOG: LazyLock<Mutex<HashMap<RuleKey, (Instant, u64)>>> =
    LazyLock::new(Default::default);

/// Returns the applications not logged since this key's last line, or None
/// while its window lasts, so a flood writes one line per address a minute.
fn admit(log: &mut HashMap<RuleKey, (Instant, u64)>, key: RuleKey, now: Instant) -> Option<u64> {
    match log.get_mut(&key) {
        Some((since, skipped)) if now.duration_since(*since) < RULE_LOG_WINDOW => {
            *skipped += 1;
            None
        }
        Some(entry) => Some(std::mem::replace(entry, (now, 0)).1),
        None => {
            // ponytail: clearing a full table can log a flooding address again within its
            // minute; an LRU would keep its entry if 4096 addresses per minute is common.
            if log.len() >= RULE_LOG_KEYS {
                log.clear();
            }
            log.insert(key, (now, 0));
            Some(0)
        }
    }
}

/// Counts a protection rule and logs it with the address it applied to. Inside
/// a session that is the client's source: an IPv4 address or an IPv6 /64.
pub fn rule(rule: &'static str, ip: Option<IpAddr>) {
    count(RULES, rule);
    let suppressed = admit(&mut RULE_LOG.lock().unwrap(), (rule, ip), Instant::now());
    if let Some(suppressed) = suppressed {
        crate::event(
            "rule_applied",
            json!({"rule": rule, "ip": ip, "suppressed": suppressed}),
        );
    }
}

fn metric(out: &mut String, name: &str, kind: &str, help: &str, value: impl std::fmt::Display) {
    let _ = writeln!(
        out,
        "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}"
    );
}

/// The resident memory in bytes and the CPU time in seconds of this process.
fn process() -> (u64, f64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let resident = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.trim().strip_suffix(" kB")?.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // utime and stime are fields 14 and 15; the command name may contain spaces.
    let ticks: u64 = stat
        .rsplit_once(')')
        .map(|(_, rest)| {
            rest.split_whitespace()
                .skip(11)
                .take(2)
                .filter_map(|value| value.parse::<u64>().ok())
                .sum()
        })
        .unwrap_or(0);
    // ponytail: assumes USER_HZ 100, Linux's value on every relay target; sysconf needs unsafe.
    (resident * 1024, ticks as f64 / 100.0)
}

pub fn render(broker: &Broker) -> String {
    let mut out = String::new();
    let admission = &broker.admission;
    metric(
        &mut out,
        "ahrara_clients_connected",
        "gauge",
        "Authenticated client identities connected now.",
        broker.connected(),
    );
    metric(
        &mut out,
        "ahrara_websocket_connections",
        "gauge",
        "Client WebSocket connections, including those not yet authenticated.",
        admission.client_connections(),
    );
    metric(
        &mut out,
        "ahrara_smtp_sessions",
        "gauge",
        "SMTP sessions open now.",
        admission.smtp_sessions(),
    );
    metric(
        &mut out,
        "ahrara_client_sessions_total",
        "counter",
        "Client sessions authenticated.",
        CLIENT_SESSIONS.load(Relaxed),
    );
    metric(
        &mut out,
        "ahrara_email_bytes_total",
        "counter",
        "Email bytes streamed to clients, counted once per message.",
        EMAIL_BYTES.load(Relaxed),
    );
    let _ = writeln!(
        out,
        "# HELP ahrara_websocket_bytes_total Encrypted WebSocket payload bytes.\n# TYPE ahrara_websocket_bytes_total counter\nahrara_websocket_bytes_total{{direction=\"in\"}} {}\nahrara_websocket_bytes_total{{direction=\"out\"}} {}",
        WEBSOCKET_IN.load(Relaxed),
        WEBSOCKET_OUT.load(Relaxed)
    );

    let labelled = LABELLED.lock().unwrap().clone();
    for ((name, words), (label, help)) in WORDS.iter().zip([
        ("result", "Messages that reached DATA, by outcome."),
        ("reason", "Recipients refused before DATA, by reason."),
        ("rule", "Protection rules applied, by rule."),
    ]) {
        let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} counter");
        let unlisted = labelled
            .keys()
            .filter(|(metric, word)| metric == name && !words.contains(word))
            .map(|(_, word)| *word);
        for word in words.iter().copied().chain(unlisted) {
            let value = labelled.get(&(*name, word)).copied().unwrap_or(0);
            let _ = writeln!(out, "{name}{{{label}=\"{word}\"}} {value}");
        }
    }

    let (resident, cpu) = process();
    metric(
        &mut out,
        "process_resident_memory_bytes",
        "gauge",
        "Resident memory size in bytes.",
        resident,
    );
    metric(
        &mut out,
        "process_cpu_seconds_total",
        "counter",
        "Total user and system CPU time spent in seconds.",
        cpu,
    );
    let started = SystemTime::now()
        .checked_sub(broker.uptime())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    metric(
        &mut out,
        "process_start_time_seconds",
        "gauge",
        "Start time of the process since the Unix epoch in seconds.",
        started.as_secs(),
    );
    out
}

/// Serves only GET /metrics; never merge it into the public router.
pub fn router(broker: Arc<Broker>) -> Router {
    Router::new()
        .route(
            "/metrics",
            get(|State(broker): State<Arc<Broker>>| async move {
                (
                    [(CONTENT_TYPE, "text/plain; version=0.0.4")],
                    render(&broker),
                )
            }),
        )
        .with_state(broker)
}

/// The current value of a labelled counter, for tests comparing before and after.
#[cfg(test)]
pub fn labelled(metric: &str, word: &str) -> u64 {
    LABELLED
        .lock()
        .unwrap()
        .iter()
        .find(|((name, value), _)| *name == metric && *value == word)
        .map_or(0, |(_, count)| *count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_logs_once_per_address_and_window_and_stays_bounded() {
        let mut log = HashMap::new();
        let start = Instant::now();
        let key = (
            "clients_per_source_limit",
            Some("192.0.2.1".parse().unwrap()),
        );

        assert_eq!(admit(&mut log, key, start), Some(0));
        assert_eq!(admit(&mut log, key, start + Duration::from_secs(1)), None);
        assert_eq!(admit(&mut log, key, start + Duration::from_secs(2)), None);
        assert_eq!(
            admit(&mut log, ("origin_not_allowed", key.1), start),
            Some(0),
            "each rule has its own window"
        );
        assert_eq!(admit(&mut log, key, start + RULE_LOG_WINDOW), Some(2));
        for n in 0..RULE_LOG_KEYS as u32 * 2 {
            admit(
                &mut log,
                ("clients_limit", Some(IpAddr::from(n.to_be_bytes()))),
                start,
            );
        }

        assert!(log.len() <= RULE_LOG_KEYS);
    }

    #[test]
    fn exposition_has_every_metric_and_no_client_detail() {
        let broker = Broker::encrypted([1; 32]);
        rule("origin_not_allowed", Some("198.51.100.7".parse().unwrap()));
        email("delivered");
        recipient_refused("offline");
        let text = render(&broker);

        for name in [
            "ahrara_clients_connected 0",
            "ahrara_websocket_connections 0",
            "ahrara_smtp_sessions 0",
            "ahrara_client_sessions_total ",
            "ahrara_email_bytes_total ",
            "ahrara_websocket_bytes_total{direction=\"in\"} ",
            "ahrara_websocket_bytes_total{direction=\"out\"} ",
            "ahrara_emails_total{result=\"delivered\"} ",
            "ahrara_recipients_refused_total{reason=\"offline\"} ",
            "ahrara_rule_hits_total{rule=\"origin_not_allowed\"} ",
            "process_resident_memory_bytes ",
            "process_cpu_seconds_total ",
            "process_start_time_seconds ",
        ] {
            assert!(
                text.contains(&format!("\n{name}")),
                "{name} missing:\n{text}"
            );
        }

        assert!(
            !text.contains("198.51.100.7"),
            "metrics never name an address"
        );
    }

    #[test]
    fn outcomes_are_exported_before_they_first_happen() {
        // Prometheus' increase() misses a series that is born at 1.
        let text = render(&Broker::encrypted([1; 32]));

        for line in [
            "ahrara_emails_total{result=\"forwarding_invalid\"} ",
            "ahrara_recipients_refused_total{reason=\"busy\"} ",
            "ahrara_rule_hits_total{rule=\"smtp_starttls_required\"} ",
        ] {
            assert!(text.contains(line), "{line} missing");
        }
    }
}
