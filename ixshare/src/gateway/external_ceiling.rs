// Dynamic per-endpoint concurrency ceiling: a slow control loop that scrapes vLLM
// `/metrics` for slugs with `metrics_url` set and moves the live `ceiling` on
// their `ConcurrencyLimiter` via AIMD. Admission itself stays a synchronous,
// local CAS in `external_endpoint.rs`; this loop only ever writes `ceiling`.

use std::collections::BTreeMap;
use std::time::Duration;

use futures::future::join_all;

use crate::gateway::external_endpoint::{ExternalEndpointMgr, ReplicaId};
use crate::gateway::http_gateway::GATEWAY_CONFIG;

const SCRAPE_TIMEOUT: Duration = Duration::from_secs(2);
const KV_HIGH_WATER: f64 = 0.90;
const FAILURE_ERROR_THRESHOLD: u32 = 10;
/// Bounds how far a single tick may drop `c` under backlog. Tuned against the
/// controller's tick interval, not a standalone constant: see
/// docs/external-endpoint-dynamic-ceiling-seed-phase1.md.
const MAX_DROP_PER_TICK: i64 = 2;

const WAITING_METRIC: &str = "vllm:num_requests_waiting";
const KV_METRIC_V1: &str = "vllm:kv_cache_usage_perc";
const KV_METRIC_V0: &str = "vllm:gpu_cache_usage_perc";

/// Phase-1 seed law for one slug's ceiling `c`. There is no upper bound: the
/// seed (`max_concurrency`) only sets the starting point, not a clamp. Decrease
/// fires on `waiting >= decrease_waiting_threshold` (sustained backlog) or
/// `waiting > 0` with KV at the wall, bounded per tick by
/// `MAX_DROP_PER_TICK` so a large inherited backlog can't collapse the ceiling
/// in one scrape. `waiting == threshold-1` with healthy KV is treated as
/// transient (hold), which preserves the old "waiting=1 is tolerated" behavior
/// at the default threshold of 2. Increase requires both
/// `local_inflight == Some(c)` (the local limiter is actually the bottleneck,
/// not an idle endpoint) and a clean queue/KV reading; the ambiguous middle
/// (unsaturated, or high KV) holds.
pub fn step(
    c: i64,
    waiting: i64,
    kv: f64,
    local_inflight: Option<i64>,
    decrease_waiting_threshold: i64,
) -> i64 {
    if waiting >= decrease_waiting_threshold || (waiting > 0 && kv >= KV_HIGH_WATER) {
        (c - waiting.min(MAX_DROP_PER_TICK)).max(1)
    } else if local_inflight == Some(c) && kv < KV_HIGH_WATER && waiting == 0 {
        c + 1
    } else {
        c
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrapedMetrics {
    pub waiting: i64,
    pub kv: f64,
}

/// Hand-rolled Prometheus text-format line scan for the two control gauges.
/// Exact-name match only: the live exposition also carries
/// `vllm:num_requests_waiting_by_reason`, which shares the waiting gauge's
/// prefix. `waiting` sums across label sets; `kv` takes the max (summing
/// percentages could exceed 1.0 and falsely pin the controller).
pub fn parse_metrics(text: &str) -> Result<ScrapedMetrics, String> {
    let mut waiting_sum: i64 = 0;
    let mut waiting_seen = false;
    let mut kv_max: f64 = 0.0;
    let mut kv_seen = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name_end = line
            .find(|c: char| c == '{' || c.is_whitespace())
            .unwrap_or(line.len());
        let name = &line[..name_end];
        if name != WAITING_METRIC && name != KV_METRIC_V1 && name != KV_METRIC_V0 {
            continue;
        }

        let rest = line[name_end..].trim_start();
        let rest = if let Some(r) = rest.strip_prefix('{') {
            match r.find('}') {
                Some(end) => r[end + 1..].trim_start(),
                None => continue,
            }
        } else {
            rest
        };
        // The value is the first token; a trailing token is an optional timestamp.
        let value_str = rest.split_whitespace().next().unwrap_or("");
        let value: f64 = value_str
            .parse()
            .map_err(|_| format!("unparseable value for {}: {:?}", name, value_str))?;

        if name == WAITING_METRIC {
            waiting_sum += value as i64;
            waiting_seen = true;
        } else {
            kv_max = kv_max.max(value);
            kv_seen = true;
        }
    }

    if !waiting_seen || !kv_seen {
        return Err("missing waiting or kv gauge in scrape".to_string());
    }
    Ok(ScrapedMetrics { waiting: waiting_sum, kv: kv_max })
}

async fn scrape(client: &reqwest::Client, url: &str) -> Result<ScrapedMetrics, String> {
    let fetch = async {
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("scrape request error: {e}"))?;
        resp.text().await.map_err(|e| format!("scrape body error: {e}"))
    };
    let text = match tokio::time::timeout(SCRAPE_TIMEOUT, fetch).await {
        Ok(r) => r?,
        Err(_) => return Err("scrape timeout".to_string()),
    };
    parse_metrics(&text)
}

#[derive(Debug, Default, Clone)]
struct SlugState {
    consecutive_failures: u32,
    pair: (i32, String),
}

fn decide(
    mut state: SlugState,
    pair: (i32, String),
    ceiling: Option<i64>,
    inflight: Option<i64>,
    decrease_waiting_threshold: i64,
    scrape_result: Result<ScrapedMetrics, String>,
) -> (SlugState, Option<(i64, i64, ScrapedMetrics)>) {
    if state.pair != pair {
        state.pair = pair.clone();
        state.consecutive_failures = 0;
    }

    match scrape_result {
        Err(_) => {
            state.consecutive_failures += 1;
            (state, None)
        }
        Ok(metrics) => {
            state.consecutive_failures = 0;
            let Some(c) = ceiling else { return (state, None) };
            let new_c = step(c, metrics.waiting, metrics.kv, inflight, decrease_waiting_threshold);
            let change = if new_c != c { Some((c, new_c, metrics)) } else { None };
            (state, change)
        }
    }
}

async fn tick(mgr: &ExternalEndpointMgr, client: &reqwest::Client, state: &mut BTreeMap<(String, ReplicaId), SlugState>) {
    let decrease_waiting_threshold =
        GATEWAY_CONFIG.externalCeilingDecreaseWaitingThreshold.max(1) as i64;
    let endpoints = mgr.List();
    let mut dynamic: Vec<_> = Vec::new();
    for ep in &endpoints {
        let has_metrics = ep.metrics_url.is_some()
            || ep.backends.as_ref()
                .and_then(crate::gateway::external_endpoint::BackendConfig::from_json)
                .map(|c| match c { crate::gateway::external_endpoint::BackendConfig::Explicit { replicas } => replicas.iter().any(|r| r.metrics_url.is_some()) })
                .unwrap_or(false);
        if has_metrics && ep.max_concurrency >= 0 {
            for r in mgr.resolved_replicas(&ep.slug) {
                if r.metrics_url.is_some() {
                    dynamic.push((ep.clone(), r));
                }
            }
        }
    }

    let dynamic_keys: std::collections::BTreeSet<(String, ReplicaId)> =
        dynamic.iter().map(|(ep, r)| (ep.slug.clone(), r.id.clone())).collect();
    state.retain(|k, _| dynamic_keys.contains(k));

    let scrapes = join_all(
        dynamic
            .iter()
            .map(|(_, r)| scrape(client, r.metrics_url.as_deref().unwrap_or(""))),
    )
    .await;

    for ((ep, replica), result) in dynamic.iter().zip(scrapes.into_iter()) {
        let pair = (ep.max_concurrency, replica.metrics_url.clone().unwrap_or_default());
        let key = (ep.slug.clone(), replica.id.clone());
        let prior = state.remove(&key).unwrap_or_default();
        let ceiling = mgr.get_ceiling(&ep.slug, &replica.id);
        let inflight = mgr.get_inflight(&ep.slug, &replica.id);
        let (next, change) =
            decide(prior, pair, ceiling, inflight, decrease_waiting_threshold, result);

        if let Some((old, new, metrics)) = change {
            mgr.set_ceiling(&ep.slug, &replica.id, new);
            info!(
                "endpoint {} replica {} ceiling {} -> {} (waiting={}, kv={:.2})",
                ep.slug, replica.id, old, new, metrics.waiting, metrics.kv
            );
        } else if next.consecutive_failures > 0 {
            warn!("endpoint {} replica {} metrics scrape failed ({} consecutive)", ep.slug, replica.id, next.consecutive_failures);
            if next.consecutive_failures == FAILURE_ERROR_THRESHOLD {
                error!("endpoint {} replica {} metrics scrape failing repeatedly, ceiling frozen", ep.slug, replica.id);
            }
        }
        state.insert(key, next);
    }

    mgr.sweep_limiters();
}

/// Spawned once per gateway. Re-reads `List()` every tick, so admin CRUD is
/// picked up without any per-endpoint task lifecycle.
pub async fn run(mgr: ExternalEndpointMgr, client: reqwest::Client, tick_interval: Duration) {
    let mut state: BTreeMap<(String, ReplicaId), SlugState> = BTreeMap::new();
    let mut interval = tokio::time::interval(tick_interval);
    loop {
        interval.tick().await;
        tick(&mgr, &client, &mut state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_DECREASE_WAITING_THRESHOLD: i64 = 2;

    #[test]
    fn step_drops_bounded_by_max_drop_per_tick_regardless_of_kv() {
        assert_eq!(step(7, 2, 0.1, None, DEFAULT_DECREASE_WAITING_THRESHOLD), 5);
        assert_eq!(step(7, 8, 0.1, None, DEFAULT_DECREASE_WAITING_THRESHOLD), 5, "drop bounded even under a large inherited backlog");
        assert_eq!(step(7, 2, 0.95, Some(7), DEFAULT_DECREASE_WAITING_THRESHOLD), 5, "decrease ignores kv/inflight once waiting >= 2");
        assert_eq!(step(1, 2, 0.0, None, DEFAULT_DECREASE_WAITING_THRESHOLD), 1, "floor at 1");
    }

    #[test]
    fn step_holds_on_transient_waiting_1_with_healthy_kv() {
        assert_eq!(step(7, 1, 0.1, None, DEFAULT_DECREASE_WAITING_THRESHOLD), 7, "waiting=1 with healthy kv is transient, hold");
        assert_eq!(step(7, 1, 0.5, Some(7), DEFAULT_DECREASE_WAITING_THRESHOLD), 7, "waiting=1 with healthy kv holds even when saturated");
        assert_eq!(step(1, 1, 0.0, None, DEFAULT_DECREASE_WAITING_THRESHOLD), 1, "holds at floor");
    }

    #[test]
    fn step_drops_on_waiting_1_when_kv_high() {
        assert_eq!(step(7, 1, 0.90, None, DEFAULT_DECREASE_WAITING_THRESHOLD), 6, "waiting=1 + kv at wall -> decrease");
        assert_eq!(step(7, 1, 0.95, None, DEFAULT_DECREASE_WAITING_THRESHOLD), 6, "waiting=1 + kv above wall -> decrease");
    }

    #[test]
    fn step_drops_on_waiting_2_regardless_of_kv() {
        assert_eq!(step(7, 2, 0.1, None, DEFAULT_DECREASE_WAITING_THRESHOLD), 5, "waiting=2 decreases even with healthy kv");
        assert_eq!(step(7, 2, 0.95, Some(7), DEFAULT_DECREASE_WAITING_THRESHOLD), 5, "waiting=2 decreases even with high kv");
    }

    #[test]
    fn step_increases_only_when_both_signals_clean() {
        assert_eq!(step(4, 0, 0.5, Some(4), DEFAULT_DECREASE_WAITING_THRESHOLD), 5);
        assert_eq!(step(16, 0, 0.1, Some(16), DEFAULT_DECREASE_WAITING_THRESHOLD), 17, "unbounded: no clamp back to the seed");
    }

    #[test]
    fn step_does_not_increase_when_waiting_1() {
        assert_eq!(step(4, 1, 0.1, Some(4), DEFAULT_DECREASE_WAITING_THRESHOLD), 4, "waiting=1 blocks increase even if saturated and kv healthy");
    }

    #[test]
    fn step_respects_configurable_waiting_threshold() {
        assert_eq!(step(7, 9, 0.1, None, 10), 7, "below threshold backlog holds when kv is healthy");
        assert_eq!(step(7, 10, 0.1, None, 10), 5, "reaching threshold triggers the bounded decrease");
    }

    #[test]
    fn step_holds_at_kv_wall() {
        assert_eq!(step(8, 0, 0.90, Some(8), DEFAULT_DECREASE_WAITING_THRESHOLD), 8);
        assert_eq!(step(8, 0, 0.99, Some(8), DEFAULT_DECREASE_WAITING_THRESHOLD), 8);
    }

    #[test]
    fn step_holds_when_local_limiter_is_not_saturated() {
        // waiting == 0 and kv healthy, but the local limiter isn't the bottleneck:
        // an idle/underloaded endpoint must not keep climbing forever.
        assert_eq!(step(8, 0, 0.1, Some(7), DEFAULT_DECREASE_WAITING_THRESHOLD), 8);
        assert_eq!(step(8, 0, 0.1, None, DEFAULT_DECREASE_WAITING_THRESHOLD), 8, "missing inflight sample must never increase");
    }

    const V1_SAMPLE: &str = r#"
# HELP vllm:num_requests_waiting Number of requests waiting.
# TYPE vllm:num_requests_waiting gauge
vllm:num_requests_waiting{engine="0",model_name="m"} 12.0
# HELP vllm:num_requests_waiting_by_reason should not double count
vllm:num_requests_waiting_by_reason{engine="0",model_name="m",reason="queue"} 999.0
vllm:kv_cache_usage_perc{engine="0",model_name="m"} 0.93
"#;

    #[test]
    fn parser_exact_name_match_not_prefix() {
        let m = parse_metrics(V1_SAMPLE).unwrap();
        assert_eq!(m.waiting, 12, "by_reason series must not contribute");
        assert!((m.kv - 0.93).abs() < 1e-9);
    }

    #[test]
    fn parser_v0_fallback_name() {
        let text = "vllm:num_requests_waiting{engine=\"0\"} 3\nvllm:gpu_cache_usage_perc{engine=\"0\"} 0.5\n";
        let m = parse_metrics(text).unwrap();
        assert_eq!(m.waiting, 3);
        assert!((m.kv - 0.5).abs() < 1e-9);
    }

    #[test]
    fn parser_sums_waiting_and_maxes_kv_across_label_sets() {
        let text = "\
vllm:num_requests_waiting{engine=\"0\"} 3\n\
vllm:num_requests_waiting{engine=\"1\"} 5\n\
vllm:kv_cache_usage_perc{engine=\"0\"} 0.60\n\
vllm:kv_cache_usage_perc{engine=\"1\"} 0.80\n";
        let m = parse_metrics(text).unwrap();
        assert_eq!(m.waiting, 8, "waiting sums across engines");
        assert!((m.kv - 0.80).abs() < 1e-9, "kv takes the max, summing would exceed 1.0");
    }

    #[test]
    fn parser_takes_first_token_not_trailing_timestamp() {
        let text = "vllm:num_requests_waiting{engine=\"0\"} 4 1690000000000\nvllm:kv_cache_usage_perc{engine=\"0\"} 0.2 1690000000000\n";
        let m = parse_metrics(text).unwrap();
        assert_eq!(m.waiting, 4);
        assert!((m.kv - 0.2).abs() < 1e-9);
    }

    #[test]
    fn parser_rejects_garbage() {
        assert!(parse_metrics("not prometheus output at all").is_err());
    }

    #[test]
    fn decide_freezes_ceiling_on_scrape_failure() {
        let state = SlugState::default();
        let pair = (16, "http://x/metrics".to_string());
        let (next, change) = decide(
            state,
            pair,
            Some(8),
            Some(8),
            DEFAULT_DECREASE_WAITING_THRESHOLD,
            Err("timeout".to_string()),
        );
        assert!(change.is_none(), "no set_ceiling call on failure");
        assert_eq!(next.consecutive_failures, 1);
    }

    #[test]
    fn decide_resumes_increase_after_failures_clear() {
        let mut state = SlugState::default();
        state.consecutive_failures = 3;
        state.pair = (16, "http://x/metrics".to_string());
        let (next, change) = decide(
            state,
            (16, "http://x/metrics".to_string()),
            Some(8),
            Some(8),
            DEFAULT_DECREASE_WAITING_THRESHOLD,
            Ok(ScrapedMetrics { waiting: 0, kv: 0.1 }),
        );
        assert_eq!(next.consecutive_failures, 0);
        assert_eq!(change, Some((8, 9, ScrapedMetrics { waiting: 0, kv: 0.1 })));
    }

    #[test]
    fn decide_resets_failure_count_when_pair_changes() {
        let mut state = SlugState::default();
        state.consecutive_failures = 5;
        state.pair = (16, "http://old/metrics".to_string());
        let (next, _) = decide(
            state,
            (16, "http://new/metrics".to_string()),
            Some(16),
            Some(16),
            DEFAULT_DECREASE_WAITING_THRESHOLD,
            Err("timeout".to_string()),
        );
        assert_eq!(next.consecutive_failures, 1, "pair change resets, then this failure counts once");
    }

    #[test]
    fn decide_holds_on_clean_scrape_when_inflight_is_none() {
        // get_inflight can race a swept limiter within the same tick; a missing
        // sample must fall through to "hold," never treated as saturated.
        let state = SlugState::default();
        let pair = (16, "http://x/metrics".to_string());
        let (_, change) = decide(
            state,
            pair,
            Some(8),
            None,
            DEFAULT_DECREASE_WAITING_THRESHOLD,
            Ok(ScrapedMetrics { waiting: 0, kv: 0.1 }),
        );
        assert!(change.is_none(), "missing inflight sample must never trigger an increase");
    }

    #[test]
    fn decide_increases_past_max_concurrency_seed() {
        let state = SlugState::default();
        let pair = (7, "http://x/metrics".to_string());
        let (_, change) = decide(
            state,
            pair,
            Some(7),
            Some(7),
            DEFAULT_DECREASE_WAITING_THRESHOLD,
            Ok(ScrapedMetrics { waiting: 0, kv: 0.1 }),
        );
        assert_eq!(change, Some((7, 8, ScrapedMetrics { waiting: 0, kv: 0.1 })), "no clamp back to the seed of 7");
    }

    #[test]
    fn decide_bounds_decrease_under_large_waiting() {
        let state = SlugState::default();
        let pair = (7, "http://x/metrics".to_string());
        let (_, change) = decide(
            state,
            pair,
            Some(7),
            Some(7),
            DEFAULT_DECREASE_WAITING_THRESHOLD,
            Ok(ScrapedMetrics { waiting: 8, kv: 0.1 }),
        );
        assert_eq!(change, Some((7, 5, ScrapedMetrics { waiting: 8, kv: 0.1 })), "bounded by MAX_DROP_PER_TICK=2");
    }
}
