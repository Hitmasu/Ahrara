use crate::protocol::Route;
use anyhow::{Result, anyhow};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv6Addr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

const MAX_SETTING: usize = 1024;
const MAX_PENALTIES: usize = 4096;
const FAILURE_WINDOW: Duration = Duration::from_secs(300);
const COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeLimits {
    pub smtp_sessions: usize,
    pub smtp_per_source: usize,
    pub offer_timeout: Duration,
    pub ack_timeout: Duration,
    pub transaction_timeout: Duration,
    pub command_timeout: Duration,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            smtp_sessions: 256,
            smtp_per_source: 16,
            offer_timeout: Duration::from_secs(5),
            ack_timeout: Duration::from_secs(30),
            transaction_timeout: Duration::from_secs(300),
            command_timeout: Duration::from_secs(120),
        }
    }
}

impl RuntimeLimits {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| {
            std::env::var_os(key)
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| anyhow!("{key} must be UTF-8"))
                })
                .transpose()
        })
    }

    fn from_lookup(mut get: impl FnMut(&str) -> Result<Option<String>>) -> Result<Self> {
        let defaults = Self::default();
        let mut number = |key: &str, default: usize, maximum: usize| -> Result<usize> {
            let value = match get(key)? {
                Some(value) => value
                    .parse::<usize>()
                    .map_err(|_| anyhow!("{key} must be an integer in 1..{maximum}"))?,
                None => default,
            };
            if !(1..=maximum).contains(&value) {
                return Err(anyhow!("{key} must be in 1..{maximum}"));
            }

            Ok(value)
        };
        let smtp_sessions = number(
            "AHRARA_SMTP_MAX_SESSIONS",
            defaults.smtp_sessions,
            MAX_SETTING,
        )?;
        let smtp_per_source = number(
            "AHRARA_SMTP_MAX_SESSIONS_PER_SOURCE",
            defaults.smtp_per_source.min(smtp_sessions),
            smtp_sessions,
        )?;
        let mut secs = |key: &str, default: Duration| -> Result<Duration> {
            Ok(Duration::from_secs(
                number(key, default.as_secs() as usize, MAX_SETTING)? as u64,
            ))
        };

        Ok(Self {
            smtp_sessions,
            smtp_per_source,
            offer_timeout: secs("AHRARA_SMTP_OFFER_TIMEOUT_SECONDS", defaults.offer_timeout)?,
            ack_timeout: secs("AHRARA_SMTP_ACK_TIMEOUT_SECONDS", defaults.ack_timeout)?,
            transaction_timeout: secs(
                "AHRARA_SMTP_TRANSACTION_TIMEOUT_SECONDS",
                defaults.transaction_timeout,
            )?,
            command_timeout: secs(
                "AHRARA_SMTP_COMMAND_TIMEOUT_SECONDS",
                defaults.command_timeout,
            )?,
        })
    }
}

#[derive(Default)]
struct Sources {
    active: usize,
    counts: HashMap<IpAddr, usize>,
}

struct Penalty {
    failures: [Instant; 3],
    count: usize,
    next_eligible: Instant,
}

impl Penalty {
    fn record(&mut self, now: Instant) {
        if now < self.next_eligible {
            return;
        }
        if self.count == self.failures.len() {
            self.count = 0;
        }

        let mut retained = 0;

        for index in 0..self.count {
            if now.duration_since(self.failures[index]) <= FAILURE_WINDOW {
                self.failures[retained] = self.failures[index];
                retained += 1;
            }
        }

        self.failures[retained] = now;
        self.count = retained + 1;
        if self.count == self.failures.len() {
            self.next_eligible = now + COOLDOWN;
        }
    }

    fn live(&self, now: Instant) -> bool {
        if self.count == self.failures.len() {
            now < self.next_eligible
        } else {
            now.duration_since(self.last_failure()) <= FAILURE_WINDOW
        }
    }

    fn last_failure(&self) -> Instant {
        self.failures[self.count - 1]
    }
}

fn source_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(value) => {
            if let Some(value) = value.to_ipv4_mapped() {
                return IpAddr::V4(value);
            }

            let mut prefix = value.segments();
            prefix[4..].fill(0);
            IpAddr::V6(Ipv6Addr::from(prefix))
        }
        value => value,
    }
}

pub struct Admission {
    settings: RuntimeLimits,
    sources: Mutex<Sources>,
    penalties: Mutex<HashMap<Route, Penalty>>,
}

impl Admission {
    pub fn new(settings: RuntimeLimits) -> Arc<Self> {
        Arc::new(Self {
            settings,
            sources: Mutex::new(Sources::default()),
            penalties: Mutex::new(HashMap::new()),
        })
    }

    pub fn settings(&self) -> &RuntimeLimits {
        &self.settings
    }

    pub fn try_source(self: &Arc<Self>, ip: IpAddr, trusted: bool) -> Option<SourcePermit> {
        let mut sources = self.sources.lock().unwrap();
        if sources.active >= self.settings.smtp_sessions {
            return None;
        }

        let source = (!trusted).then(|| source_key(ip));
        if let Some(source) = source {
            if sources.counts.get(&source).copied().unwrap_or(0) >= self.settings.smtp_per_source {
                return None;
            }

            *sources.counts.entry(source).or_default() += 1;
        }

        sources.active += 1;
        Some(SourcePermit {
            admission: self.clone(),
            source,
        })
    }

    pub fn allows_route(&self, route: &Route) -> bool {
        self.penalties
            .lock()
            .unwrap()
            .get(route)
            .is_none_or(|penalty| Instant::now() >= penalty.next_eligible)
    }

    // Call only for a missing delivery ACK, not a NACK or a failed transport write.
    pub fn note_timeout(&self, route: Route) {
        let mut penalties = self.penalties.lock().unwrap();
        let now = Instant::now();
        if let Some(penalty) = penalties.get_mut(&route) {
            penalty.record(now);
            return;
        }
        if penalties.len() >= MAX_PENALTIES {
            penalties.retain(|_, penalty| penalty.live(now));
            if penalties.len() >= MAX_PENALTIES {
                // ponytail: bounded oldest-entry eviction can forget an offence under
                // sustained route churn; a full table never blocks unrelated recipients.
                let oldest = *penalties
                    .iter()
                    .min_by_key(|(_, p)| p.last_failure())
                    .unwrap()
                    .0;

                penalties.remove(&oldest);
            }
        }

        let mut penalty = Penalty {
            failures: [now; 3],
            count: 0,
            next_eligible: now,
        };
        penalty.record(now);
        penalties.insert(route, penalty);
    }

    pub fn note_success(&self, route: &Route) {
        self.penalties.lock().unwrap().remove(route);
    }

    // The Broker's existing 30-second sweep calls this; there is no per-session timer.
    pub fn sweep(&self) {
        let mut penalties = self.penalties.lock().unwrap();
        let now = Instant::now();
        penalties.retain(|_, penalty| penalty.live(now));
    }
}

pub struct SourcePermit {
    admission: Arc<Admission>,
    source: Option<IpAddr>,
}

impl Drop for SourcePermit {
    fn drop(&mut self) {
        let mut sources = self.admission.sources.lock().unwrap();
        sources.active -= 1;
        if let Some(source) = self.source {
            let count = sources.counts.get_mut(&source).unwrap();
            *count -= 1;
            if *count == 0 {
                sources.counts.remove(&source);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(values: &[(&str, &str)]) -> Result<RuntimeLimits> {
        RuntimeLimits::from_lookup(|key| {
            Ok(values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).into()))
        })
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    fn route(value: u32) -> Route {
        let mut route = [0; 16];
        route[..4].copy_from_slice(&value.to_be_bytes());
        route
    }

    #[test]
    fn settings_validate_every_bound_without_mutating_the_environment() {
        assert_eq!(configured(&[]).unwrap(), RuntimeLimits::default());
        let small = configured(&[("AHRARA_SMTP_MAX_SESSIONS", "2")]).unwrap();

        assert_eq!(small.smtp_sessions, 2);
        assert_eq!(small.smtp_per_source, 2);
        let large = configured(&[
            ("AHRARA_SMTP_MAX_SESSIONS", "1024"),
            ("AHRARA_SMTP_MAX_SESSIONS_PER_SOURCE", "1024"),
            ("AHRARA_SMTP_ACK_TIMEOUT_SECONDS", "10"),
        ])
        .unwrap();

        assert_eq!(large.smtp_sessions, 1024);
        assert_eq!(large.smtp_per_source, 1024);
        assert_eq!(large.ack_timeout, Duration::from_secs(10));

        for key in [
            "AHRARA_SMTP_MAX_SESSIONS",
            "AHRARA_SMTP_MAX_SESSIONS_PER_SOURCE",
            "AHRARA_SMTP_OFFER_TIMEOUT_SECONDS",
            "AHRARA_SMTP_ACK_TIMEOUT_SECONDS",
            "AHRARA_SMTP_TRANSACTION_TIMEOUT_SECONDS",
            "AHRARA_SMTP_COMMAND_TIMEOUT_SECONDS",
        ] {
            for value in ["0", "1025", "-1", "forever", ""] {
                assert!(
                    configured(&[(key, value)]).is_err(),
                    "accepted invalid {key}"
                );
            }
        }

        assert!(
            configured(&[
                ("AHRARA_SMTP_MAX_SESSIONS", "2"),
                ("AHRARA_SMTP_MAX_SESSIONS_PER_SOURCE", "3"),
            ])
            .is_err()
        );
    }

    #[test]
    fn untrusted_source_quota_releases_on_drop_without_blocking_other_sources() {
        let admission = Admission::new(RuntimeLimits {
            smtp_sessions: 3,
            smtp_per_source: 1,
            ..RuntimeLimits::default()
        });
        let slow = admission.try_source(ip("192.0.2.1"), false).unwrap();

        assert!(admission.try_source(ip("192.0.2.1"), false).is_none());
        let fast = admission.try_source(ip("192.0.2.2"), false).unwrap();

        assert_eq!(admission.sources.lock().unwrap().counts.len(), 2);
        drop(slow);
        let replacement = admission.try_source(ip("192.0.2.1"), false).unwrap();
        drop(replacement);
        drop(fast);
        let state = admission.sources.lock().unwrap();

        assert_eq!(state.active, 0);
        assert!(state.counts.is_empty());
    }

    #[tokio::test]
    async fn aborted_session_releases_global_and_source_quota() {
        let admission = Admission::new(RuntimeLimits {
            smtp_sessions: 1,
            smtp_per_source: 1,
            ..RuntimeLimits::default()
        });
        let source = ip("192.0.2.1");
        let permit = admission.try_source(source, false).unwrap();
        let task = tokio::spawn(async move {
            let _permit = permit;
            std::future::pending::<()>().await;
        });

        assert!(admission.try_source(source, false).is_none());
        task.abort();

        assert!(task.await.unwrap_err().is_cancelled());
        assert!(admission.sources.lock().unwrap().counts.is_empty());
        assert!(admission.try_source(source, false).is_some());
    }

    #[test]
    fn trusted_shared_forwarder_bypasses_only_source_quota_and_rejected_ips_are_not_saved() {
        let admission = Admission::new(RuntimeLimits {
            smtp_sessions: 2,
            smtp_per_source: 1,
            ..RuntimeLimits::default()
        });
        let first = admission.try_source(ip("192.0.2.1"), true).unwrap();
        let second = admission.try_source(ip("192.0.2.1"), true).unwrap();

        for n in 2..=200 {
            assert!(
                admission
                    .try_source(IpAddr::from([192, 0, 2, n]), false)
                    .is_none()
            );
        }

        assert_eq!(admission.sources.lock().unwrap().active, 2);
        assert!(admission.sources.lock().unwrap().counts.is_empty());
        drop(first);
        let untrusted = admission.try_source(ip("192.0.2.2"), false).unwrap();
        drop(second);
        drop(untrusted);

        assert_eq!(admission.sources.lock().unwrap().active, 0);
    }

    #[test]
    fn mapped_ipv4_and_ipv6_privacy_addresses_share_their_source_quota() {
        let admission = Admission::new(RuntimeLimits {
            smtp_sessions: 8,
            smtp_per_source: 1,
            ..RuntimeLimits::default()
        });
        let ipv4 = admission.try_source(ip("192.0.2.5"), false).unwrap();

        assert!(
            admission
                .try_source(ip("::ffff:192.0.2.5"), false)
                .is_none()
        );
        let ipv6 = admission.try_source(ip("2001:db8:1:2::1"), false).unwrap();

        assert!(
            admission
                .try_source(ip("2001:db8:1:2::abcd"), false)
                .is_none()
        );
        let other = admission.try_source(ip("2001:db8:1:3::1"), false).unwrap();

        assert_eq!(admission.sources.lock().unwrap().counts.len(), 3);
        drop((ipv4, ipv6, other));

        assert!(admission.sources.lock().unwrap().counts.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn third_ack_timeout_penalizes_only_the_same_route_across_reconnections() {
        let admission = Admission::new(RuntimeLimits::default());
        let slow = route(1);
        let fast = route(2);

        for _ in 0..2 {
            admission.note_timeout(slow);
            assert!(admission.allows_route(&slow));
        }

        let reconnected_broker_reference = admission.clone();
        reconnected_broker_reference.note_timeout(slow);

        assert!(!admission.allows_route(&slow));
        assert!(admission.allows_route(&fast));
        tokio::time::advance(Duration::from_secs(59)).await;
        admission.note_timeout(slow); // An already in-flight failure cannot extend the cooldown.
        assert!(!admission.allows_route(&slow));
        tokio::time::advance(Duration::from_secs(1)).await;

        assert!(admission.allows_route(&slow));
        admission.sweep();

        assert!(admission.penalties.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_threshold_uses_a_sliding_window_not_fixed_buckets() {
        let admission = Admission::new(RuntimeLimits::default());
        let route = route(3);
        admission.note_timeout(route); // 0
        tokio::time::advance(Duration::from_secs(299)).await;
        admission.note_timeout(route); // 299
        tokio::time::advance(Duration::from_secs(2)).await;
        admission.note_timeout(route); // 301, timeout 0 has expired
        assert!(admission.allows_route(&route));
        tokio::time::advance(Duration::from_secs(1)).await;
        admission.note_timeout(route); // 302: three failures within three seconds
        assert!(!admission.allows_route(&route));
    }

    #[tokio::test(start_paused = true)]
    async fn successful_ack_resets_failures_and_expired_offences_are_swept() {
        let admission = Admission::new(RuntimeLimits::default());
        let route = route(4);
        admission.note_timeout(route);
        admission.note_timeout(route);
        admission.note_success(&route);

        assert!(admission.penalties.lock().unwrap().is_empty());
        admission.note_timeout(route);

        assert!(admission.allows_route(&route));
        tokio::time::advance(FAILURE_WINDOW + Duration::from_secs(1)).await;
        admission.sweep();

        assert!(admission.penalties.lock().unwrap().is_empty());

        for _ in 0..3 {
            admission.note_timeout(route);
        }

        assert!(!admission.allows_route(&route));
        admission.note_success(&route);

        assert!(admission.allows_route(&route));
    }

    #[tokio::test(start_paused = true)]
    async fn offence_table_stays_bounded_and_overflow_does_not_deny_unrelated_routes() {
        let admission = Admission::new(RuntimeLimits::default());
        for value in 0..MAX_PENALTIES as u32 {
            admission.note_timeout(route(value));
            tokio::time::advance(Duration::from_millis(1)).await;
        }

        assert_eq!(admission.penalties.lock().unwrap().len(), MAX_PENALTIES);
        admission.note_timeout(route(MAX_PENALTIES as u32));
        {
            let penalties = admission.penalties.lock().unwrap();
            assert_eq!(penalties.len(), MAX_PENALTIES);
            assert!(!penalties.contains_key(&route(0)));
            assert!(penalties.contains_key(&route(MAX_PENALTIES as u32)));
        }

        assert!(admission.allows_route(&route(9000)));
        tokio::time::advance(FAILURE_WINDOW + Duration::from_secs(1)).await;
        admission.sweep();

        assert!(admission.penalties.lock().unwrap().is_empty());
    }
}
