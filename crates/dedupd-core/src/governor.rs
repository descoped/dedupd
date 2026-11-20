//! Paces what dedupd asks of the provider.
//!
//! Three things decide how hard it may push. What the server says wins:
//! `Retry-After` stops every request for that long, and the `RateLimit`
//! headers give a budget that requests are spread over when it runs low.
//! Where the server gives no number (timeouts, resets, a slow download
//! server), the number of downloads in flight follows AIMD: one more after
//! each clean round, half after trouble. Last, the owner's own limits: a
//! ceiling on connections and an optional ceiling on bytes per second.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

/// Connections to use at most when `max_connections` is not set.
pub const DEFAULT_CONNECTIONS: usize = 8;
/// Connections a fresh governor starts with.
const START_WINDOW: f64 = 2.0;
/// The longest pause a server may ask for.
const MAX_HOLD: Duration = Duration::from_secs(900);
/// The longest a budget is believed to take to fill again, whatever the
/// server's header says. Hugging Face's windows are five minutes.
const MAX_RESET: Duration = Duration::from_secs(3600);
/// The window is halved at most once in this time, so one bad moment that
/// fails several requests counts once.
const CUT_INTERVAL: Duration = Duration::from_secs(1);

/// What a request is for; each kind has its own budget at the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Metadata: model info, file listings, search.
    Api,
    /// File content.
    Files,
}

impl Class {
    /// The name Hugging Face gives this budget in its `RateLimit` headers.
    fn budget(self) -> &'static str {
        match self {
            Class::Api => "api",
            Class::Files => "resolvers",
        }
    }
}

#[derive(Debug, Clone)]
struct Budget {
    quota: u64,
    remaining: u64,
    resets_at: Instant,
}

struct State {
    window: f64,
    max: usize,
    in_flight: usize,
    hold_until: Option<Instant>,
    last_cut: Option<Instant>,
    budgets: HashMap<String, Budget>,
    /// When the next request of a class may go out while its budget is low.
    next_slot: HashMap<&'static str, Instant>,
    /// Bytes per second; 0 is no limit.
    speed_limit: u64,
    tokens: f64,
    refilled: Instant,
}

/// Paces every request to the provider: how many downloads run at once,
/// how long to hold back when the provider says so, and the owner's
/// ceilings on connections and bytes per second. One per engine, shared by
/// the hub client and the fetcher.
pub struct Governor {
    state: Mutex<State>,
    freed: Notify,
}

/// One download in flight. Dropping it frees the slot.
pub struct Permit(Option<Arc<Governor>>);

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(governor) = self.0.take() {
            governor.lock().in_flight -= 1;
            governor.freed.notify_one();
        }
    }
}

enum Wait {
    For(Duration),
    Slot,
}

/// The longest one sleep for the speed limit, so a limit the owner raises
/// or takes away is noticed within this time.
const THROTTLE_STEP: Duration = Duration::from_secs(1);

impl Governor {
    /// A governor with the owner's ceilings. No connections means one;
    /// a speed limit of 0 is no limit.
    pub fn new(max_connections: usize, speed_limit: u64) -> Arc<Self> {
        let max = max_connections.max(1);
        Arc::new(Self {
            state: Mutex::new(State {
                window: START_WINDOW.min(max as f64),
                max,
                in_flight: 0,
                hold_until: None,
                last_cut: None,
                budgets: HashMap::new(),
                next_slot: HashMap::new(),
                speed_limit,
                tokens: speed_limit as f64,
                refilled: Instant::now(),
            }),
            freed: Notify::new(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("governor lock")
    }

    /// Wait until a request of `class` may go out. For file content the
    /// returned permit holds one of the connections until it is dropped.
    /// A caller that stops waiting (its future is dropped) takes no place,
    /// and a permit can be dropped at any time: its place goes to a waiter.
    pub async fn permit(self: &Arc<Self>, class: Class) -> Permit {
        loop {
            let wait = {
                let mut s = self.lock();
                let now = Instant::now();
                if let Some(wait) = s.must_wait(class, now) {
                    Wait::For(wait)
                } else if class == Class::Files && s.in_flight >= s.allowed() {
                    Wait::Slot
                } else {
                    s.spend(class, now);
                    if class == Class::Api {
                        return Permit(None);
                    }
                    s.in_flight += 1;
                    return Permit(Some(self.clone()));
                }
            };
            match wait {
                Wait::For(d) => tokio::time::sleep(d).await,
                // A wake-up can be missed between the check and the wait, so
                // the wait is also bounded.
                Wait::Slot => {
                    let _ = tokio::time::timeout(Duration::from_millis(250), self.freed.notified())
                        .await;
                }
            }
        }
    }

    /// A download finished cleanly: allow a little more.
    pub fn succeeded(&self) {
        let mut s = self.lock();
        s.window = (s.window + 1.0 / s.window).min(s.max as f64);
        drop(s);
        self.freed.notify_one();
    }

    /// A request failed in a way that says "slow down". With `retry_after`
    /// the server named a pause, which every request then waits out. Returns
    /// how long requests are held from now.
    pub fn congested(&self, retry_after: Option<Duration>) -> Duration {
        let mut s = self.lock();
        let now = Instant::now();
        if s.last_cut
            .is_none_or(|t| now.duration_since(t) >= CUT_INTERVAL)
        {
            s.window = (s.window / 2.0).max(1.0);
            s.last_cut = Some(now);
        }
        if let Some(pause) = retry_after {
            let until = now + pause.min(MAX_HOLD);
            if s.hold_until.is_none_or(|h| h < until) {
                s.hold_until = Some(until);
            }
        }
        s.hold_until
            .map(|h| h.saturating_duration_since(now))
            .unwrap_or_default()
    }

    /// Read the provider's `RateLimit` headers off an answer.
    pub fn observe(&self, headers: &reqwest::header::HeaderMap) {
        let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        let Some((name, remaining, reset)) = text("ratelimit").and_then(parse_ratelimit) else {
            return;
        };
        let quota = text("ratelimit-policy")
            .and_then(parse_policy)
            .filter(|(n, _)| *n == name)
            .map(|(_, q)| q);
        let mut s = self.lock();
        // The server names its budgets; ones that have reset are let go,
        // so the map holds only what is in force.
        let now = Instant::now();
        s.budgets.retain(|_, b| b.resets_at > now);
        let known = s.budgets.get(&name).map(|b| b.quota);
        s.budgets.insert(
            name,
            Budget {
                quota: quota.or(known).unwrap_or(remaining),
                remaining,
                // A number from a header: cut to a time that can be added.
                resets_at: Instant::now() + Duration::from_secs(reset).min(MAX_RESET),
            },
        );
    }

    /// Wait as long as the speed limit asks for `bytes` that just arrived.
    /// The wait is taken in steps, and the limit is read again at each: a
    /// limit that was raised or taken away meanwhile shortens or ends it.
    pub async fn throttle(&self, bytes: usize) {
        // Counted once, on the first turn.
        let mut arrived = bytes as f64;
        loop {
            let wait = {
                let mut s = self.lock();
                if s.speed_limit == 0 {
                    return;
                }
                let limit = s.speed_limit as f64;
                let now = Instant::now();
                let gained = now.duration_since(s.refilled).as_secs_f64() * limit;
                s.tokens = (s.tokens + gained).min(limit);
                s.refilled = now;
                s.tokens -= std::mem::take(&mut arrived);
                if s.tokens >= 0.0 {
                    return;
                }
                Duration::from_secs_f64(-s.tokens / limit)
            };
            if wait <= THROTTLE_STEP {
                tokio::time::sleep(wait).await;
                return;
            }
            tokio::time::sleep(THROTTLE_STEP).await;
        }
    }

    /// Change the owner's limits while running.
    pub fn configure(&self, max_connections: usize, speed_limit: u64) {
        let mut s = self.lock();
        s.max = max_connections.max(1);
        s.window = s.window.min(s.max as f64);
        s.speed_limit = speed_limit;
        s.tokens = s.tokens.min(speed_limit as f64);
        drop(s);
        self.freed.notify_one();
    }

    /// The owner's ceiling on downloads at once; at least 1.
    pub fn max_connections(&self) -> usize {
        self.lock().max
    }

    /// How hard the daemon is asking right now, for the owner to see.
    pub fn state(&self) -> dedupd_api::jobs::Governor {
        let s = self.lock();
        let now = Instant::now();
        let mut budgets: Vec<dedupd_api::jobs::Budget> = s
            .budgets
            .iter()
            .filter(|(_, b)| b.resets_at > now)
            .map(|(name, b)| dedupd_api::jobs::Budget {
                name: name.clone(),
                quota: b.quota,
                remaining: b.remaining,
                resets_in: b.resets_at.duration_since(now).as_secs(),
            })
            .collect();
        budgets.sort_by(|a, b| a.name.cmp(&b.name));
        dedupd_api::jobs::Governor {
            connections: s.in_flight as u64,
            window: s.allowed() as u64,
            max_connections: s.max as u64,
            hold_seconds: s
                .hold_until
                .map(|h| h.saturating_duration_since(now).as_secs())
                .unwrap_or(0),
            speed_limit: s.speed_limit,
            budgets,
        }
    }
}

impl State {
    fn allowed(&self) -> usize {
        (self.window.floor() as usize).clamp(1, self.max)
    }

    /// How long a request of `class` has to wait for the server's sake.
    fn must_wait(&mut self, class: Class, now: Instant) -> Option<Duration> {
        if let Some(until) = self.hold_until {
            if until > now {
                return Some(until - now);
            }
            self.hold_until = None;
        }
        let budget = self.budgets.get(class.budget())?;
        if budget.resets_at <= now {
            return None;
        }
        if budget.remaining == 0 {
            return Some(budget.resets_at - now);
        }
        let next = *self.next_slot.get(class.budget())?;
        (next > now).then(|| next - now)
    }

    /// Count one request against its budget, and when the budget is nearly
    /// used up, spread what is left over the time until it is full again.
    fn spend(&mut self, class: Class, now: Instant) {
        let Some(budget) = self.budgets.get_mut(class.budget()) else {
            return;
        };
        if budget.resets_at <= now {
            return;
        }
        budget.remaining = budget.remaining.saturating_sub(1);
        if budget.remaining.saturating_mul(10) < budget.quota {
            let left = budget.resets_at - now;
            let gap = left / u32::try_from(budget.remaining.max(1)).unwrap_or(u32::MAX);
            self.next_slot.insert(class.budget(), now + gap);
        } else {
            self.next_slot.remove(class.budget());
        }
    }
}

/// `"api";r=499;t=209`: the budget's name, what is left, seconds to reset.
fn parse_ratelimit(value: &str) -> Option<(String, u64, u64)> {
    let mut parts = value.split(';').map(str::trim);
    let name = parts.next()?.trim_matches('"').to_string();
    let (mut remaining, mut reset) = (None, None);
    for part in parts {
        match part.split_once('=') {
            Some(("r", v)) => remaining = v.parse().ok(),
            Some(("t", v)) => reset = v.parse().ok(),
            _ => {}
        }
    }
    Some((name, remaining?, reset?))
}

/// `"fixed window";"api";q=500;w=300`: the budget's name and its size.
fn parse_policy(value: &str) -> Option<(String, u64)> {
    let mut name = None;
    let mut quota = None;
    for part in value.split(';').map(str::trim) {
        if let Some(("q", v)) = part.split_once('=') {
            quota = v.parse().ok();
        } else if part.starts_with('"') {
            name = Some(part.trim_matches('"').to_string());
        }
    }
    Some((name?, quota?))
}

/// The pause a `Retry-After` header asks for, when it is given in seconds.
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let seconds: u64 = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(*k, HeaderValue::from_static(v));
        }
        map
    }

    #[test]
    fn reads_the_rate_limit_headers() {
        assert_eq!(
            parse_ratelimit(r#""api";r=499;t=209"#),
            Some(("api".into(), 499, 209))
        );
        assert_eq!(
            parse_policy(r#""fixed window";"api";q=500;w=300"#),
            Some(("api".into(), 500))
        );
        assert_eq!(parse_ratelimit("nonsense"), None);
        let h = headers(&[("retry-after", "42")]);
        assert_eq!(retry_after(&h), Some(Duration::from_secs(42)));
    }

    #[tokio::test(start_paused = true)]
    async fn window_grows_by_one_per_round_and_halves_on_trouble() {
        let g = Governor::new(8, 0);
        assert_eq!(g.state().window, 2);
        // Each success adds its share of one connection, so a round of
        // them (about as many as the window is wide) adds one.
        g.succeeded();
        g.succeeded();
        assert_eq!(g.state().window, 2);
        g.succeeded();
        assert_eq!(g.state().window, 3);
        for _ in 0..40 {
            g.succeeded();
        }
        assert_eq!(g.state().window, 8, "never above the owner's limit");

        g.congested(None);
        assert_eq!(g.state().window, 4);
        g.congested(None);
        assert_eq!(g.state().window, 4, "one bad moment counts once");
        tokio::time::advance(Duration::from_secs(2)).await;
        g.congested(None);
        assert_eq!(g.state().window, 2);
        g.congested(None);
        tokio::time::advance(Duration::from_secs(2)).await;
        g.congested(None);
        assert_eq!(g.state().window, 1, "never below one");
    }

    #[tokio::test(start_paused = true)]
    async fn permits_are_limited_to_the_window() {
        let g = Governor::new(8, 0);
        let a = g.permit(Class::Files).await;
        let _b = g.permit(Class::Files).await;
        assert_eq!(g.state().connections, 2);
        let third = tokio::spawn({
            let g = g.clone();
            async move { g.permit(Class::Files).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!third.is_finished(), "the window is two");
        drop(a);
        let _c = third.await.unwrap();
        assert_eq!(g.state().connections, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_owner_s_ceiling_bounds_the_window() {
        let g = Governor::new(1, 0);
        let _a = g.permit(Class::Files).await;
        let waiting = tokio::spawn({
            let g = g.clone();
            async move { g.permit(Class::Files).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!waiting.is_finished(), "the ceiling is one");
        // A higher ceiling does not open more connections by itself: the
        // window grows as downloads finish cleanly, up to the new ceiling.
        g.configure(8, 0);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!waiting.is_finished());
        g.succeeded();
        let _b = waiting.await.unwrap();
        assert_eq!(g.state().connections, 2);
        // A lower one cuts the window at once, and never below one.
        g.configure(0, 0);
        assert_eq!((g.max_connections(), g.state().window), (1, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiter_that_goes_away_takes_no_place_and_a_dropped_permit_gives_its_own() {
        let g = Governor::new(1, 0);
        let held = g.permit(Class::Files).await;
        let gone = tokio::spawn({
            let g = g.clone();
            async move { g.permit(Class::Files).await }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        gone.abort();
        let _ = gone.await;
        assert_eq!(g.state().connections, 1);
        // A download that is ended drops its permit; the next one runs.
        let running = tokio::spawn(async move {
            let _held = held;
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        running.abort();
        let _ = running.await;
        let _next = g.permit(Class::Files).await;
        assert_eq!(g.state().connections, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_download_waiting_out_the_speed_limit_notices_a_new_one() {
        // 10 bytes a second, and 1000 bytes just arrived: 99 seconds owed.
        let g = Governor::new(8, 10);
        let started = Instant::now();
        let waiting = tokio::spawn({
            let g = g.clone();
            async move { g.throttle(1000).await }
        });
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!waiting.is_finished());
        // The owner takes the limit away.
        g.configure(8, 0);
        waiting.await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_budget_that_has_reset_is_let_go() {
        let g = Governor::new(8, 0);
        let headers = |name: &str| {
            let mut h = reqwest::header::HeaderMap::new();
            h.insert(
                "ratelimit",
                format!(r#""{name}";r=5;t=10"#).parse().unwrap(),
            );
            h
        };
        g.observe(&headers("pages"));
        g.observe(&headers("api"));
        assert_eq!(g.lock().budgets.len(), 2);
        tokio::time::sleep(Duration::from_secs(11)).await;
        g.observe(&headers("resolvers"));
        let names: Vec<String> = g.lock().budgets.keys().cloned().collect();
        assert_eq!(names, ["resolvers"]);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_holds_every_request() {
        let g = Governor::new(8, 0);
        let held = g.congested(Some(Duration::from_secs(40)));
        assert_eq!(held, Duration::from_secs(40));
        assert_eq!(g.state().hold_seconds, 40);
        let started = Instant::now();
        let _p = g.permit(Class::Api).await;
        assert!(started.elapsed() >= Duration::from_secs(40));
        assert_eq!(g.state().hold_seconds, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn numbers_no_server_should_send_do_no_harm() {
        let g = Governor::new(8, 0);
        // A reset time that cannot be added to a clock, and a count that
        // does not fit the arithmetic done with it.
        for (limit, policy) in [
            (
                r#""api";r=0;t=18446744073709551615"#,
                r#""p";"api";q=500;w=300"#,
            ),
            (
                r#""api";r=4294967296;t=18446744073709551615"#,
                r#""p";"api";q=99999999999999999999;w=1"#,
            ),
            (
                r#""api";r=4294967296;t=60"#,
                r#""p";"api";q=18446744073709551615;w=1"#,
            ),
            (
                r#""api";r=18446744073709551615;t=60"#,
                r#""p";"api";q=18446744073709551615;w=1"#,
            ),
        ] {
            g.observe(&headers(&[
                ("ratelimit", limit),
                ("ratelimit-policy", policy),
            ]));
            let started = Instant::now();
            let _p = g.permit(Class::Api).await;
            // Never held longer than a budget is believed to take to fill
            // again, whatever was claimed.
            assert!(started.elapsed() <= MAX_RESET, "{limit}");
            let _ = g.state();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_empty_budget_waits_for_its_reset_and_a_low_one_is_spread() {
        let g = Governor::new(8, 0);
        g.observe(&headers(&[
            ("ratelimit", r#""api";r=0;t=30"#),
            ("ratelimit-policy", r#""fixed window";"api";q=500;w=300"#),
        ]));
        let started = Instant::now();
        let _p = g.permit(Class::Api).await;
        assert!(started.elapsed() >= Duration::from_secs(30));

        // Ten left of five hundred with a hundred seconds to go: one request
        // about every ten seconds.
        g.observe(&headers(&[
            ("ratelimit", r#""api";r=10;t=100"#),
            ("ratelimit-policy", r#""fixed window";"api";q=500;w=300"#),
        ]));
        let started = Instant::now();
        let _a = g.permit(Class::Api).await;
        let _b = g.permit(Class::Api).await;
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_secs(10) && waited <= Duration::from_secs(13),
            "{waited:?}"
        );
        // The other budget is untouched.
        let started = Instant::now();
        let _f = g.permit(Class::Files).await;
        assert!(started.elapsed() < Duration::from_secs(1));
        let state = g.state();
        assert_eq!(state.budgets[0].name, "api");
        assert_eq!(state.budgets[0].quota, 500);
    }

    #[tokio::test(start_paused = true)]
    async fn the_speed_limit_spreads_bytes_over_time() {
        let g = Governor::new(8, 1000);
        let started = Instant::now();
        g.throttle(1000).await;
        assert!(
            started.elapsed() < Duration::from_millis(10),
            "one second's worth is free"
        );
        g.throttle(3000).await;
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_secs(3) && waited < Duration::from_secs(4),
            "{waited:?}"
        );
    }
}
