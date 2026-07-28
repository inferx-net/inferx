// Dynamic per-endpoint concurrency ceiling: a slow control loop that scrapes vLLM
// `/metrics` for slugs with `metrics_url` set and moves the live `ceiling` on
// their `ConcurrencyLimiter` via AIMD. Admission itself stays a synchronous,
// local CAS in `external_endpoint.rs`; this loop only ever writes `ceiling`.

use std::collections::BTreeMap;
use std::time::Duration;

use futures::future::join_all;

use crate::gateway::external_endpoint::ExternalEndpointMgr;

const SCRAPE_TIMEOUT: Duration = Duration::from_secs(2);
const KV_HIGH_WATER: f64 = 0.90;
const FAILURE_ERROR_THRESHOLD: u32 = 10;

const WAITING_METRIC: &str = "vllm:num_requests_waiting";
const KV_METRIC_V1: &str = "vllm:kv_cache_usage_perc";
const KV_METRIC_V0: &str = "vllm:gpu_cache_usage_perc";

/// AIMD law for one slug's ceiling `c`, bounded to `[1, n]`. Decrease fires on
/// `waiting > 0` alone; increase requires both signals clean; the ambiguous
/// middle (empty queue, high KV) holds.
pub fn step(c: i64, n: i64, waiting: i64, kv: f64) -> i64 {
    let n = n.max(1);
    if waiting > 0 {
        (c / 2).max(1)
    } else if kv < KV_HIGH_WATER {
        (c + 1).min(n)
    } else {
        c.clamp(1, n)
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

#[derive(Debug, Default)]
struct SlugState {
    consecutive_failures: u32,
    pair: (i32, String),
}

/// Task-local decision for one slug on one tick: which `SlugState` to carry
/// forward, and (when a successful scrape moved the ceiling) the log line's
/// `(old, new)` values.
fn decide(
    mut state: SlugState,
    pair: (i32, String),
    ceiling: Option<i64>,
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
            let n = pair.0 as i64;
            let new_c = step(c, n, metrics.waiting, metrics.kv);
            let change = if new_c != c { Some((c, new_c, metrics)) } else { None };
            (state, change)
        }
    }
}

async fn tick(mgr: &ExternalEndpointMgr, client: &reqwest::Client, state: &mut BTreeMap<String, SlugState>) {
    let endpoints = mgr.List();
    let dynamic: Vec<_> = endpoints.into_iter().filter(|e| e.metrics_url.is_some()).collect();

    let dynamic_slugs: std::collections::BTreeSet<&str> = dynamic.iter().map(|e| e.slug.as_str()).collect();
    state.retain(|slug, _| dynamic_slugs.contains(slug.as_str()));

    let scrapes = join_all(
        dynamic
            .iter()
            .map(|ep| scrape(client, ep.metrics_url.as_deref().unwrap_or(""))),
    )
    .await;

    for (ep, result) in dynamic.iter().zip(scrapes.into_iter()) {
        let pair = (ep.max_concurrency, ep.metrics_url.clone().unwrap_or_default());
        let prior = state.remove(&ep.slug).unwrap_or_default();
        let ceiling = mgr.get_ceiling(&ep.slug);
        let (next, change) = decide(prior, pair, ceiling, result);

        if let Some((old, new, metrics)) = change {
            mgr.set_ceiling(&ep.slug, new);
            info!(
                "endpoint {} ceiling {} -> {} (waiting={}, kv={:.2})",
                ep.slug, old, new, metrics.waiting, metrics.kv
            );
        } else if next.consecutive_failures > 0 {
            warn!("endpoint {} metrics scrape failed ({} consecutive)", ep.slug, next.consecutive_failures);
            if next.consecutive_failures == FAILURE_ERROR_THRESHOLD {
                error!("endpoint {} metrics scrape failing repeatedly, ceiling frozen", ep.slug);
            }
        }
        state.insert(ep.slug.clone(), next);
    }

    mgr.sweep_limiters();
}

/// Spawned once per gateway. Re-reads `List()` every tick, so admin CRUD is
/// picked up without any per-endpoint task lifecycle.
pub async fn run(mgr: ExternalEndpointMgr, client: reqwest::Client, tick_interval: Duration) {
    let mut state: BTreeMap<String, SlugState> = BTreeMap::new();
    let mut interval = tokio::time::interval(tick_interval);
    loop {
        interval.tick().await;
        tick(&mgr, &client, &mut state).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_halves_on_waiting_regardless_of_kv() {
        assert_eq!(step(8, 16, 12, 0.1), 4);
        assert_eq!(step(8, 16, 12, 0.95), 4);
        assert_eq!(step(1, 16, 1, 0.0), 1, "floor at 1");
    }

    #[test]
    fn step_increases_only_when_both_signals_clean() {
        assert_eq!(step(4, 16, 0, 0.5), 5);
        assert_eq!(step(16, 16, 0, 0.1), 16, "clamped at N");
    }

    #[test]
    fn step_holds_at_kv_wall() {
        assert_eq!(step(8, 16, 0, 0.90), 8);
        assert_eq!(step(8, 16, 0, 0.99), 8);
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
        let (next, change) = decide(state, pair, Some(8), Err("timeout".to_string()));
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
            Err("timeout".to_string()),
        );
        assert_eq!(next.consecutive_failures, 1, "pair change resets, then this failure counts once");
    }
}
