// External OpenAI-compatible endpoints: a Postgres table mirrored in full into a
// gateway BTreeMap, kept exact by write-through on every admin write. Serving
// reads never touch the DB. Coherent for a single gateway only.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::sync::RwLock;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use futures::StreamExt;
use hyper::body::Bytes;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::common::*;
use crate::gateway::http_gateway::GATEWAY_CONFIG;
use crate::gateway::secret::{ExternalEndpoint, SqlSecret};
use serde::Deserialize;

/// Upstream liveness deadlines for the outbound proxy (seconds). Passed in by the
/// caller — global gateway-config defaults in production, explicit values in tests —
/// so `proxy_to_external` does not depend on the global config being initialized.
/// No total-request-time cap by design, so long streaming generations never sever.
/// The connect timeout is not here: it is baked into the shared `reqwest::Client`
/// at gateway startup and cannot vary per request.
#[derive(Debug, Clone, Copy)]
pub struct ExternalTimeouts {
    pub response_header_secs: u64,
    pub idle_secs: u64,
}

impl ExternalTimeouts {
    pub fn from_gateway_config() -> Self {
        Self {
            response_header_secs: GATEWAY_CONFIG.externalResponseHeaderTimeoutSecs,
            idle_secs: GATEWAY_CONFIG.externalIdleTimeoutSecs,
        }
    }
}

/// Slug charset shared by classification and admin create/edit. Rejecting `/`,
/// whitespace, and URI-special chars keeps the single-kind 404 invariant airtight
/// and avoids the raw `Uri::try_from(...).unwrap()` panic on the serving path.
pub fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReplicaId(pub String);

impl std::fmt::Display for ReplicaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveredReplica {
    pub id: ReplicaId,
    pub base_url: String,
    pub metrics_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReplicaConfig {
    pub base_url: String,
    #[serde(default)]
    pub metrics_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "discovery")]
pub enum BackendConfig {
    #[serde(rename = "explicit")]
    Explicit {
        replicas: Vec<ReplicaConfig>,
    },
}

impl BackendConfig {
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        serde_json::from_value(v.clone()).ok()
    }
}

pub fn resolve_replicas(ep: &ExternalEndpoint) -> Vec<DiscoveredReplica> {
    if let Some(cfg) = ep.backends.as_ref().and_then(BackendConfig::from_json) {
        match cfg {
            BackendConfig::Explicit { replicas } => {
                replicas.iter().map(|r| DiscoveredReplica {
                    id: ReplicaId(r.base_url.clone()),
                    base_url: r.base_url.clone(),
                    metrics_url: r.metrics_url.clone(),
                }).collect()
            }
        }
    } else {
        vec![DiscoveredReplica {
            id: ReplicaId("default".to_string()),
            base_url: ep.base_url.clone(),
            metrics_url: ep.metrics_url.clone(),
        }]
    }
}

/// Atomic admission primitive: `inflight < ceiling` admits, CAS'd. A resize
/// (either direction) is a plain store, visible to the very next admission.
#[derive(Debug)]
pub struct ConcurrencyLimiter {
    inflight: AtomicI64,
    ceiling: AtomicI64,
}

impl ConcurrencyLimiter {
    fn new(ceiling: i64) -> Self {
        Self {
            inflight: AtomicI64::new(0),
            ceiling: AtomicI64::new(ceiling),
        }
    }
}

/// `None` = unlimited endpoint, no per-slug state, drop is a no-op.
#[derive(Debug)]
pub struct ConcurrencyPermit(Option<Arc<ConcurrencyLimiter>>);

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        if let Some(limiter) = &self.0 {
            limiter.inflight.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExternalEndpointMgr {
    sql: SqlSecret,
    map: Arc<RwLock<BTreeMap<String, ExternalEndpoint>>>,
    client: reqwest::Client,
    limiters: Arc<RwLock<BTreeMap<(String, ReplicaId), Arc<ConcurrencyLimiter>>>>,
    resolved_backends: Arc<RwLock<BTreeMap<String, Vec<DiscoveredReplica>>>>,
}

impl ExternalEndpointMgr {
    /// Hydrate the full mirror from Postgres at gateway startup.
    pub async fn New(sql: SqlSecret, client: reqwest::Client) -> Result<Self> {
        let rows = sql.LoadExternalEndpoints().await?;
        let mut resolved_backends = BTreeMap::new();
        let mut limiters = BTreeMap::new();
        let mut map = BTreeMap::new();
        for row in &rows {
            map.insert(row.slug.clone(), row.clone());
            let replicas = resolve_replicas(row);
            if row.max_concurrency >= 0 {
                for r in &replicas {
                    limiters.insert(
                        (row.slug.clone(), r.id.clone()),
                        Arc::new(ConcurrencyLimiter::new(row.max_concurrency as i64)),
                    );
                }
            }
            resolved_backends.insert(row.slug.clone(), replicas);
        }
        Ok(Self {
            sql,
            map: Arc::new(RwLock::new(map)),
            client,
            limiters: Arc::new(RwLock::new(limiters)),
            resolved_backends: Arc::new(RwLock::new(resolved_backends)),
        })
    }

    pub fn HttpClient(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn resolved_replicas(&self, slug: &str) -> Vec<DiscoveredReplica> {
        self.resolved_backends.read().unwrap().get(slug).cloned().unwrap_or_default()
    }

    /// Admission gate for one upstream request to `slug`. `Some(permit)` = admitted
    /// (held for the request's lifetime, released on drop); `None` = a capped
    /// endpoint at capacity. `max_concurrency < 0` is unlimited: always `Some`.
    /// `max_concurrency == 0` rejects every request.
    pub fn try_acquire(&self, slug: &str, replica_id: &ReplicaId) -> Option<ConcurrencyPermit> {
        let max = self
            .map
            .read()
            .unwrap()
            .get(slug)
            .map(|e| e.max_concurrency)
            .unwrap_or(-1);
        try_acquire_slot(max, slug, replica_id, &self.limiters)
    }

    /// Read fresh each controller tick; the controller never caches `c`.
    pub fn get_ceiling(&self, slug: &str, replica_id: &ReplicaId) -> Option<i64> {
        self.limiters
            .read()
            .unwrap()
            .get(&(slug.to_string(), replica_id.clone()))
            .map(|l| l.ceiling.load(Ordering::SeqCst))
    }

    pub fn set_ceiling(&self, slug: &str, replica_id: &ReplicaId, c: i64) {
        let map = self.map.read().unwrap();
        let Some(ep) = map.get(slug) else { return };
        let cfg = ep.backends.as_ref().and_then(BackendConfig::from_json);
        if let Some(_cfg) = cfg {
            let resolved = self.resolved_backends.read().unwrap();
            let Some(replica) = resolved.get(slug).and_then(|rs| rs.iter().find(|r| &r.id == replica_id)) else {
                return;
            };
            if replica.metrics_url.is_none() {
                return;
            }
        } else if ep.metrics_url.is_none() {
            return;
        }
        let floored = c.max(1);
        if let Some(limiter) = self.limiters.read().unwrap().get(&(slug.to_string(), replica_id.clone())) {
            limiter.ceiling.store(floored, Ordering::SeqCst);
        }
    }

    pub fn get_inflight(&self, slug: &str, replica_id: &ReplicaId) -> Option<i64> {
        self.limiters
            .read()
            .unwrap()
            .get(&(slug.to_string(), replica_id.clone()))
            .map(|l| l.inflight.load(Ordering::SeqCst))
    }

    /// Drops drained (`inflight == 0`) entries with no mirror row or an
    /// unlimited row. Runs every controller tick regardless of dynamic slugs.
    pub fn sweep_limiters(&self) {
        let map = self.map.read().unwrap();
        let mut limiters = self.limiters.write().unwrap();
        limiters.retain(|(slug, _), limiter| {
            if limiter.inflight.load(Ordering::SeqCst) != 0 {
                return true;
            }
            match map.get(slug) {
                None => false,
                Some(ep) => ep.max_concurrency >= 0,
            }
        });
    }

    pub fn Get(&self, slug: &str) -> Option<ExternalEndpoint> {
        self.map.read().unwrap().get(slug).cloned()
    }

    pub fn Contains(&self, slug: &str) -> bool {
        self.map.read().unwrap().contains_key(slug)
    }

    pub fn List(&self) -> Vec<ExternalEndpoint> {
        self.map.read().unwrap().values().cloned().collect()
    }

    /// Row mutation and ceiling reset share the map's write lock with
    /// `set_ceiling`'s guard. On a change to `max_concurrency` or `metrics_url`
    /// the ceiling resets to `N`; a dynamic row's limiter is eagerly created even
    /// without a change, so the controller can act pre-traffic. `-1` stores
    /// nothing — unlimited bypasses the limiter and it drains until swept.
    fn upsert_mirror(&self, ep: ExternalEndpoint) {
        let old = {
            let map = self.map.read().unwrap();
            map.get(&ep.slug).cloned()
        };
        let max_changed = old.as_ref().map(|o| o.max_concurrency) != Some(ep.max_concurrency);
        let metrics_changed = old.as_ref().map(|o| o.metrics_url.clone()) != Some(ep.metrics_url.clone());
        let backends_changed = old.as_ref().map(|o| o.backends.clone()) != Some(ep.backends.clone());
        let changed = max_changed || metrics_changed || backends_changed;

        let cfg = ep.backends.as_ref().and_then(BackendConfig::from_json);
        let has_metrics = ep.metrics_url.is_some()
            || cfg.as_ref().map(|c| match c { BackendConfig::Explicit { replicas } => replicas.iter().any(|r| r.metrics_url.is_some()) }).unwrap_or(false);

        if cfg.is_some() && backends_changed {
            let resolved = resolve_replicas(&ep);
            if ep.max_concurrency >= 0 {
                let ceiling = ep.max_concurrency as i64;
                let new_ids: std::collections::HashSet<&ReplicaId> = resolved.iter().map(|r| &r.id).collect();
                {
                    let mut limiters = self.limiters.write().unwrap();
                    limiters.retain(|(s, rid), l| {
                        s != &ep.slug || l.inflight.load(Ordering::SeqCst) != 0 || new_ids.contains(rid)
                    });
                    for r in &resolved {
                        limiters.entry((ep.slug.clone(), r.id.clone()))
                            .and_modify(|l| { l.ceiling.store(ceiling, Ordering::SeqCst); })
                            .or_insert_with(|| Arc::new(ConcurrencyLimiter::new(ceiling)));
                    }
                }
            }
            self.resolved_backends.write().unwrap().insert(ep.slug.clone(), resolved);
        } else if has_metrics && ep.max_concurrency >= 0 {
            let ceiling = ep.max_concurrency as i64;
            if max_changed || metrics_changed {
                let resolved = self.resolved_backends.read().unwrap().get(&ep.slug).cloned()
                    .unwrap_or_else(|| resolve_replicas(&ep));
                if self.resolved_backends.read().unwrap().get(&ep.slug).is_none() {
                    self.resolved_backends.write().unwrap().insert(ep.slug.clone(), resolved.clone());
                }
                let mut limiters = self.limiters.write().unwrap();
                for r in &resolved {
                    limiters.entry((ep.slug.clone(), r.id.clone()))
                        .and_modify(|l| { if max_changed || metrics_changed { l.ceiling.store(ceiling, Ordering::SeqCst); } })
                        .or_insert_with(|| Arc::new(ConcurrencyLimiter::new(ceiling)));
                }
            }
        } else if changed && ep.max_concurrency >= 0 {
            let ceiling = ep.max_concurrency as i64;
            let mut limiters = self.limiters.write().unwrap();
            for ((_, _), l) in limiters.iter_mut().filter(|((s, _), _)| s == &ep.slug) {
                l.ceiling.store(ceiling, Ordering::SeqCst);
            }
        }

        // Guarantee the serving path always has replicas. A no-backends,
        // no-metrics row falls through every branch above, so materialize the
        // default replica (idempotent when a branch already inserted).
        if self.resolved_backends.read().unwrap().get(&ep.slug).is_none() {
            self.resolved_backends
                .write()
                .unwrap()
                .insert(ep.slug.clone(), resolve_replicas(&ep));
        }

        self.map.write().unwrap().insert(ep.slug.clone(), ep);
    }

    pub async fn Create(
        &self,
        slug: &str,
        base_url: &str,
        upstream_model: &str,
        provider_api_key: &str,
        max_concurrency: i32,
        metrics_url: Option<&str>,
        backends: Option<&serde_json::Value>,
    ) -> Result<ExternalEndpoint> {
        let ep = self
            .sql
            .InsertExternalEndpoint(slug, base_url, upstream_model, provider_api_key, max_concurrency, metrics_url, backends)
            .await?;
        self.upsert_mirror(ep.clone());
        Ok(ep)
    }

    pub async fn Update(
        &self,
        slug: &str,
        base_url: &str,
        upstream_model: &str,
        provider_api_key: Option<&str>,
        max_concurrency: i32,
        metrics_url: Option<&str>,
        backends: Option<&serde_json::Value>,
    ) -> Result<ExternalEndpoint> {
        let ep = self
            .sql
            .UpdateExternalEndpoint(slug, base_url, upstream_model, provider_api_key, max_concurrency, metrics_url, backends)
            .await?;
        self.upsert_mirror(ep.clone());
        Ok(ep)
    }

    pub async fn SetPublished(
        &self,
        slug: &str,
        published: bool,
        by: &str,
    ) -> Result<ExternalEndpoint> {
        let ep = self
            .sql
            .SetExternalEndpointPublished(slug, published, by)
            .await?;
        self.upsert_mirror(ep.clone());
        Ok(ep)
    }

    /// Leaves the limiter entry in place (tombstoned): a same-slug recreate
    /// reuses the counter instead of orphaning it. A slug with no mirror row
    /// admits nothing anyway. The sweep prunes it once drained.
    pub async fn Delete(&self, slug: &str) -> Result<()> {
        self.sql.DeleteExternalEndpoint(slug).await?;
        self.map.write().unwrap().remove(slug);
        self.resolved_backends.write().unwrap().remove(slug);
        Ok(())
    }
}

fn external_gateway_error(code: StatusCode, msg: String) -> Response {
    Response::builder().status(code).body(Body::from(msg)).unwrap()
}

/// Split out so it is testable without a DB-backed mgr. `max < 0` is unlimited;
/// otherwise the per-slug limiter is looked up (or lazily created) and CAS-admitted.
fn try_acquire_slot(
    max: i32,
    slug: &str,
    replica_id: &ReplicaId,
    limiters: &RwLock<BTreeMap<(String, ReplicaId), Arc<ConcurrencyLimiter>>>,
) -> Option<ConcurrencyPermit> {
    if max < 0 {
        return Some(ConcurrencyPermit(None));
    }
    let key = (slug.to_string(), replica_id.clone());
    let limiter = {
        let read = limiters.read().unwrap();
        read.get(&key).cloned()
    };
    let limiter = match limiter {
        Some(l) => l,
        None => limiters
            .write()
            .unwrap()
            .entry(key)
            .or_insert_with(|| Arc::new(ConcurrencyLimiter::new(max as i64)))
            .clone(),
    };
    loop {
        let inflight = limiter.inflight.load(Ordering::SeqCst);
        let ceiling = limiter.ceiling.load(Ordering::SeqCst);
        if inflight >= ceiling {
            return None;
        }
        if limiter
            .inflight
            .compare_exchange(inflight, inflight + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Some(ConcurrencyPermit(Some(limiter)));
        }
    }
}

/// Stream a call to the provider with connect/response-header/idle deadlines (not reqwest's
/// total-duration `.timeout()`) and adapt the reqwest response into an axum one.
/// Fresh outbound headers only: the caller's Authorization is never forwarded.
/// `sub_path` has the `/v1` root stripped (base_url carries it).
pub async fn proxy_to_external(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    sub_path: &str,
    body_bytes: Vec<u8>,
    timeouts: ExternalTimeouts,
) -> std::result::Result<Response, (StatusCode, String)> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), sub_path);

    let send = client
        .post(&url)
        .header("content-type", "application/json")
        .bearer_auth(api_key)
        .body(body_bytes)
        .send();

    let resp = match tokio::time::timeout(
        std::time::Duration::from_secs(timeouts.response_header_secs),
        send,
    )
    .await
    {
        Err(_) => {
            return Ok(external_gateway_error(
                StatusCode::GATEWAY_TIMEOUT,
                "service failure: upstream response header timeout".to_string(),
            ))
        }
        Ok(Err(e)) => {
            if e.is_connect() {
                return Err((
                    StatusCode::BAD_GATEWAY,
                    format!("service failure: upstream connect error: {e}"),
                ));
            }
            let code = if e.is_timeout() {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
            return Ok(external_gateway_error(
                code,
                format!("service failure: upstream transport error: {e}"),
            ));
        }
        Ok(Ok(r)) => r,
    };

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut builder = Response::builder().status(status);
    for (k, v) in resp.headers().iter() {
        if k.as_str().eq_ignore_ascii_case("content-length") {
            continue;
        }
        // reqwest (http 0.2) -> axum (http 1.x): rebuild from bytes across versions.
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_str().as_bytes()),
            HeaderValue::from_bytes(v.as_bytes()),
        ) {
            builder = builder.header(name, val);
        }
    }

    // Idle deadline: bound the gap BETWEEN chunks. A stall truncates the stream
    // (surfaced as a body error -> not billed). Healthy long generations are unaffected.
    let idle = std::time::Duration::from_secs(timeouts.idle_secs);
    let (tx, rx) = mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(128);
    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        loop {
            match tokio::time::timeout(idle, stream.next()).await {
                Err(_) => {
                    let _ = tx
                        .send(Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "upstream idle timeout",
                        )))
                        .await;
                    return;
                }
                Ok(None) => return,
                Ok(Some(Ok(bytes))) => {
                    if tx.send(Ok(bytes)).await.is_err() {
                        return;
                    }
                }
                Ok(Some(Err(e))) => {
                    let _ = tx
                        .send(Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            e.to_string(),
                        )))
                        .await;
                    return;
                }
            }
        }
    });

    Ok(builder
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap())
}

/// Provider-facing sub-path for an incoming shared-surface request path.
///
/// Two front doors reach the same upstream: OpenRouter arrives as `/v1/...` and the
/// direct surface as `/endpoints/v1/...`. Stripping `/endpoints` normalizes both to
/// `/v1/...`; stripping `/v1` then yields the sub-path, because the stored `base_url`
/// already carries the provider's `/v1` root. So `/chat/completions` and `/completions`
/// both survive verbatim and are simply concatenated onto `base_url`.
pub fn external_sub_path(incoming_path: &str) -> String {
    let remain = incoming_path.strip_prefix("/endpoints").unwrap_or(incoming_path);
    remain.strip_prefix("/v1").unwrap_or(remain).to_string()
}

/// Published-gate decision for the direct `/endpoints/v1/...` surface. An external
/// endpoint has no `funcstatus`, so it serves only when its own `published` bit is set.
pub fn external_published_gate(ext: &ExternalEndpoint) -> std::result::Result<(), String> {
    if ext.published {
        Ok(())
    } else {
        Err(format!("endpoint {} is unpublished", ext.slug))
    }
}

/// The exact kind-branch `validate_agent_endpoint_published` takes, factored out so the
/// whole decision (not just the inner gate) is testable without a gateway:
/// - `Some(Ok/Err)` when the slug is an external endpoint (serve iff published);
/// - `None` when it is not external, signaling the caller to fall through to
///   self-hosted func resolution (which is what enforces the raw-path 404 invariant —
///   an external slug never reaches func resolution, and a non-external slug is resolved
///   as a func exactly as before).
pub fn endpoint_published_gate(
    ext: Option<&ExternalEndpoint>,
) -> Option<std::result::Result<(), String>> {
    ext.map(external_published_gate)
}

/// `/endpoints/v1/models` entries for the published external endpoints. The func
/// enumeration that backs `SharedEndpointModels` is blind to external endpoints (they
/// have no func), so these are merged in explicitly. Unpublished endpoints are omitted.
pub fn external_model_entries(
    endpoints: &[ExternalEndpoint],
    created: i64,
) -> Vec<serde_json::Value> {
    endpoints
        .iter()
        .filter(|ext| ext.published)
        .map(|ext| {
            serde_json::json!({
                "id": ext.slug,
                "object": "model",
                "created": created,
                "owned_by": "inferx",
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(slug: &str, published: bool) -> ExternalEndpoint {
        ExternalEndpoint {
            slug: slug.to_string(),
            base_url: "https://api.provider.com/v1".to_string(),
            upstream_model: "u".to_string(),
            provider_api_key: "sk-secret".to_string(),
            published,
            max_concurrency: -1,
            last_published_by: None,
            metrics_url: None,
            backends: None,
        }
    }

    #[test]
    fn published_gate_serves_only_when_published() {
        assert!(external_published_gate(&ep("m", true)).is_ok());
        let err = external_published_gate(&ep("m", false)).unwrap_err();
        assert!(err.contains("unpublished"), "got: {err}");
    }

    #[test]
    fn published_gate_branches_on_kind() {
        // External + published → serve.
        assert!(endpoint_published_gate(Some(&ep("m", true))).unwrap().is_ok());
        // External + unpublished → reject (404 on the direct surface).
        assert!(endpoint_published_gate(Some(&ep("m", false))).unwrap().is_err());
        // Not external → None → caller falls through to self-hosted func resolution.
        // This is the branch that keeps external endpoints off the func path and lets a
        // non-external slug 404 through the ordinary func lookup.
        assert!(endpoint_published_gate(None).is_none());
    }

    #[test]
    fn model_entries_include_only_published() {
        let eps = vec![ep("pub-a", true), ep("unpub", false), ep("pub-b", true)];
        let entries = external_model_entries(&eps, 42);
        let ids: Vec<&str> = entries
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        // Unpublished is omitted; published carry the slug as id and the passed `created`.
        assert_eq!(ids, vec!["pub-a", "pub-b"]);
        assert!(entries.iter().all(|e| e["created"].as_i64() == Some(42)));
        assert!(entries.iter().all(|e| e["object"] == "model"));
        assert!(entries.iter().all(|e| e["owned_by"] == "inferx"));
    }

    fn empty_limiters() -> RwLock<BTreeMap<(String, ReplicaId), Arc<ConcurrencyLimiter>>> {
        RwLock::new(BTreeMap::new())
    }

    #[test]
    fn limiter_caps_at_max_and_admits_below() {
        let limiters = empty_limiters();

        let p1 = try_acquire_slot(2, "m", &default_replica_id(), &limiters);
        let p2 = try_acquire_slot(2, "m", &default_replica_id(), &limiters);
        assert!(p1.is_some() && p2.is_some(), "two permits fit under cap 2");
        // At capacity: rejected.
        assert!(try_acquire_slot(2, "m", &default_replica_id(), &limiters).is_none());
        // Releasing one frees a slot for a new request.
        drop(p1);
        assert!(try_acquire_slot(2, "m", &default_replica_id(), &limiters).is_some());
    }

    #[test]
    fn limiter_unlimited_always_admits_without_state() {
        let limiters = empty_limiters();
        let mut held = Vec::new();
        for _ in 0..1000 {
            let p = try_acquire_slot(-1, "m", &default_replica_id(), &limiters);
            assert!(p.is_some(), "max -1 is unlimited");
            held.push(p);
        }
        // No per-slug limiter is allocated for an unlimited endpoint.
        assert!(limiters.read().unwrap().is_empty());
    }

    #[test]
    fn limiter_zero_rejects_every_request() {
        let limiters = empty_limiters();
        // 0 is a real cap, not the unlimited sentinel: nothing is ever admitted,
        // which is the 429 kill-switch state.
        assert!(try_acquire_slot(0, "m", &default_replica_id(), &limiters).is_none());
        assert!(try_acquire_slot(0, "m", &default_replica_id(), &limiters).is_none());
    }

    #[test]
    fn limiter_shrink_applies_to_next_admission_with_no_overadmission() {
        let limiters = empty_limiters();

        let _p1 = try_acquire_slot(3, "m", &default_replica_id(), &limiters);
        let _p2 = try_acquire_slot(3, "m", &default_replica_id(), &limiters);
        // Shrink while 2 are in flight: an atomic store, no remove-and-rebuild.
        limiters.read().unwrap().get(&("m".to_string(), default_replica_id())).unwrap().ceiling.store(1, Ordering::SeqCst);
        // The next admission sees the new ceiling immediately: already over it,
        // so it is rejected outright (never transiently admits old + new).
        assert!(try_acquire_slot(3, "m", &default_replica_id(), &limiters).is_none());
    }

    #[test]
    fn limiter_grow_admits_immediately() {
        let limiters = empty_limiters();

        let _p1 = try_acquire_slot(1, "m", &default_replica_id(), &limiters);
        assert!(try_acquire_slot(1, "m", &default_replica_id(), &limiters).is_none(), "cap 1 full");
        limiters.read().unwrap().get(&("m".to_string(), default_replica_id())).unwrap().ceiling.store(3, Ordering::SeqCst);
        assert!(try_acquire_slot(1, "m", &default_replica_id(), &limiters).is_some());
        assert!(try_acquire_slot(1, "m", &default_replica_id(), &limiters).is_some());
    }

    // ---- ExternalEndpointMgr: contracts 1 and 4 (no DB needed by these methods) ----

    fn test_mgr() -> ExternalEndpointMgr {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://user:pass@localhost/db")
            .unwrap();
        ExternalEndpointMgr {
            sql: SqlSecret { pool },
            map: Arc::new(RwLock::new(BTreeMap::new())),
            client: reqwest::Client::new(),
            limiters: Arc::new(RwLock::new(BTreeMap::new())),
            resolved_backends: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    fn default_replica_id() -> ReplicaId {
        ReplicaId("default".to_string())
    }

    fn dyn_ep(slug: &str, max_concurrency: i32, metrics_url: Option<&str>) -> ExternalEndpoint {
        ExternalEndpoint {
            slug: slug.to_string(),
            base_url: "http://vllm:8000/v1".to_string(),
            upstream_model: "u".to_string(),
            provider_api_key: "sk-secret".to_string(),
            published: true,
            max_concurrency,
            last_published_by: None,
            metrics_url: metrics_url.map(|s| s.to_string()),
            backends: None,
        }
    }

    #[tokio::test]
    async fn upsert_materializes_default_replica_for_plain_static_row() {
        // Regression: a no-backends, no-metrics row created at runtime left
        // resolved_backends empty, so the serving path returned 503
        // "no available replicas". The default replica must be materialized
        // from base_url.
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", -1, None));
        let replicas = mgr.resolved_replicas("m");
        assert_eq!(replicas.len(), 1);
        assert_eq!(replicas[0].id, default_replica_id());
        assert_eq!(replicas[0].base_url, "http://vllm:8000/v1");

        // A second upsert (e.g. admin edits max_concurrency only) must not
        // clobber the existing resolved entry.
        mgr.upsert_mirror(dyn_ep("m", 4, None));
        let replicas = mgr.resolved_replicas("m");
        assert_eq!(replicas.len(), 1, "re-upsert preserves the default replica");
    }

    #[tokio::test]
    async fn upsert_materializes_default_replica_for_static_capped_row() {
        // Same regression for a static row with a finite cap and no metrics:
        // the ceiling branch above does not insert into resolved_backends.
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 8, None));
        assert_eq!(mgr.resolved_replicas("m").len(), 1);
    }

    #[tokio::test]
    async fn eager_limiter_exists_before_first_request_and_honors_lowered_ceiling() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 16, Some("http://x/metrics")));
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(16));

        mgr.set_ceiling("m", &default_replica_id(), 4);
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(mgr.try_acquire("m", &default_replica_id()).expect("within lowered ceiling"));
        }
        assert!(mgr.try_acquire("m", &default_replica_id()).is_none(), "5th request rejected at 4, not N=16");
    }

    #[tokio::test]
    async fn dynamic_to_static_transition_restores_ceiling_to_n() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 16, Some("http://x/metrics")));
        mgr.set_ceiling("m", &default_replica_id(), 2);
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(2));

        // Admin clears metrics_url with the same max_concurrency.
        mgr.upsert_mirror(dyn_ep("m", 16, None));
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(16));
    }

    #[tokio::test]
    async fn mirror_upsert_resets_ceiling_on_metrics_url_set_or_edit() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 16, None));
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), None, "static row: lazy, no limiter yet");

        mgr.upsert_mirror(dyn_ep("m", 16, Some("http://x/metrics")));
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(16));

        mgr.set_ceiling("m", &default_replica_id(), 3);
        mgr.upsert_mirror(dyn_ep("m", 16, Some("http://y/metrics")));
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(16), "editing metrics_url resets too");
    }

    #[tokio::test]
    async fn upsert_never_overwrites_with_a_stale_cached_ceiling() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 2, Some("http://x/metrics")));
        mgr.set_ceiling("m", &default_replica_id(), 2);

        // Admin raises N to 16; the reset must be visible to the very next read.
        mgr.upsert_mirror(dyn_ep("m", 16, Some("http://x/metrics")));
        let c = mgr.get_ceiling("m", &default_replica_id()).unwrap();
        assert_eq!(c, 16, "must read the reset value, not a value derived from the pre-edit c=2");
    }

    #[tokio::test]
    async fn tombstone_continuity_recreate_reuses_the_draining_limiter() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 2, Some("http://x/metrics")));
        let held = mgr.try_acquire("m", &default_replica_id()).expect("admits within cap 2");

        // Simulate Delete: drop the row, leave the limiter tombstoned (the row-gate
        // that would otherwise refuse a rowless slug lives in dispatch, not here).
        mgr.map.write().unwrap().remove("m");

        // Recreate at the same slug with a smaller cap: the still-draining permit
        // counts against the new ceiling (never old_inflight + new_cap).
        mgr.upsert_mirror(dyn_ep("m", 1, Some("http://x/metrics")));
        assert!(mgr.try_acquire("m", &default_replica_id()).is_none(), "1 already in flight fills cap 1");
        drop(held);
        assert!(mgr.try_acquire("m", &default_replica_id()).is_some());
    }

    #[tokio::test]
    async fn sweep_prunes_drained_rowless_and_unlimited_entries_only() {
        let mgr = test_mgr();

        mgr.upsert_mirror(dyn_ep("m", 2, Some("http://x/metrics")));
        let held = mgr.try_acquire("m", &default_replica_id()).expect("admits within cap 2");
        mgr.map.write().unwrap().remove("m");
        mgr.sweep_limiters();
        assert!(mgr.limiters.read().unwrap().contains_key(&("m".to_string(), default_replica_id())), "still draining: not pruned");
        drop(held);
        mgr.sweep_limiters();
        assert!(!mgr.limiters.read().unwrap().contains_key(&("m".to_string(), default_replica_id())), "drained + no row: pruned");

        // Recreated as unlimited: no ceiling store, but the old limiter still
        // drains and must be swept once empty even though a row exists.
        mgr.upsert_mirror(dyn_ep("u", 2, Some("http://y/metrics")));
        let held2 = mgr.try_acquire("u", &default_replica_id()).expect("admits within cap 2");
        mgr.upsert_mirror(dyn_ep("u", -1, None));
        mgr.sweep_limiters();
        assert!(mgr.limiters.read().unwrap().contains_key(&("u".to_string(), default_replica_id())), "still draining: not pruned");
        drop(held2);
        mgr.sweep_limiters();
        assert!(!mgr.limiters.read().unwrap().contains_key(&("u".to_string(), default_replica_id())), "drained + unlimited row: pruned");
    }

    #[tokio::test]
    async fn guarded_set_ceiling_drops_write_after_metrics_url_cleared() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 16, Some("http://x/metrics")));
        // Racing tick's store was computed while the slug was still dynamic...
        let pre_edit_ceiling = mgr.get_ceiling("m", &default_replica_id()).unwrap();
        // ...but the admin's clear lands first, restoring the static cap.
        mgr.upsert_mirror(dyn_ep("m", 16, None));
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(16));

        mgr.set_ceiling("m", &default_replica_id(), pre_edit_ceiling / 2);
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(16), "guard drops the write: metrics_url is cleared");
    }

    #[tokio::test]
    async fn set_ceiling_no_longer_clamps_to_max_concurrency_for_dynamic_rows() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 4, Some("http://x/metrics")));
        // Phase-1: max_concurrency is only the seed, not an upper bound; the
        // controller may raise the live ceiling past it.
        mgr.set_ceiling("m", &default_replica_id(), 10);
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(10), "no clamp to max_concurrency=4");
    }

    #[tokio::test]
    async fn set_ceiling_still_floors_at_one() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 4, Some("http://x/metrics")));
        mgr.set_ceiling("m", &default_replica_id(), -3);
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(1), "floors at 1 even for a negative drop");
    }

    #[tokio::test]
    async fn get_inflight_tracks_held_permits_and_none_for_unknown_slug() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 4, Some("http://x/metrics")));
        assert_eq!(mgr.get_inflight("m", &default_replica_id()), Some(0));

        let p1 = mgr.try_acquire("m", &default_replica_id()).expect("within cap");
        let p2 = mgr.try_acquire("m", &default_replica_id()).expect("within cap");
        assert_eq!(mgr.get_inflight("m", &default_replica_id()), Some(2));

        drop(p1);
        assert_eq!(mgr.get_inflight("m", &default_replica_id()), Some(1));
        drop(p2);

        assert_eq!(mgr.get_inflight("no-such-slug", &default_replica_id()), None);
    }

    #[tokio::test]
    async fn upsert_resets_controller_raised_ceiling_on_max_concurrency_edit() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 7, Some("http://x/metrics")));
        // Controller raised the live ceiling above the seed.
        mgr.set_ceiling("m", &default_replica_id(), 20);
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(20));

        mgr.upsert_mirror(dyn_ep("m", 9, Some("http://x/metrics")));
        assert_eq!(
            mgr.get_ceiling("m", &default_replica_id()),
            Some(9),
            "editing max_concurrency resets a controller-raised ceiling back to the new seed"
        );
    }

    #[tokio::test]
    async fn upsert_resets_controller_raised_ceiling_on_metrics_url_edit() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 7, Some("http://x/metrics")));
        mgr.set_ceiling("m", &default_replica_id(), 20);
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(20));

        mgr.upsert_mirror(dyn_ep("m", 7, Some("http://y/metrics")));
        assert_eq!(
            mgr.get_ceiling("m", &default_replica_id()),
            Some(7),
            "editing metrics_url resets a controller-raised ceiling back to the seed"
        );
    }

    #[tokio::test]
    async fn upsert_restores_static_cap_after_controller_raised_ceiling_above_seed() {
        let mgr = test_mgr();
        mgr.upsert_mirror(dyn_ep("m", 7, Some("http://x/metrics")));
        mgr.set_ceiling("m", &default_replica_id(), 20);
        assert_eq!(mgr.get_ceiling("m", &default_replica_id()), Some(20));

        mgr.upsert_mirror(dyn_ep("m", 7, None));
        assert_eq!(
            mgr.get_ceiling("m", &default_replica_id()),
            Some(7),
            "disabling dynamic mode restores the static cap, even from a controller-raised ceiling"
        );
    }

    #[test]
    fn slug_charset() {
        assert!(is_valid_slug("gpt-4o_mini.v1"));
        assert!(is_valid_slug("Model123"));
        // Rejected: empty, slash, whitespace, and URI-special characters.
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("a/b"));
        assert!(!is_valid_slug("../etc"));
        assert!(!is_valid_slug("a b"));
        assert!(!is_valid_slug("a?b=1"));
        assert!(!is_valid_slug("a#b"));
        assert!(!is_valid_slug("a\nb"));
    }

    // ---- external_sub_path: both front doors, both OpenAI routes ----

    #[test]
    fn sub_path_strips_endpoints_and_v1_for_both_routes() {
        // Direct surface: `/endpoints` then `/v1` come off, leaving the sub-path that is
        // concatenated onto a base_url already ending in the provider's `/v1` root.
        assert_eq!(
            external_sub_path("/endpoints/v1/chat/completions"),
            "/chat/completions"
        );
        assert_eq!(
            external_sub_path("/endpoints/v1/completions"),
            "/completions"
        );

        // OpenRouter surface arrives without the `/endpoints` prefix.
        assert_eq!(external_sub_path("/v1/chat/completions"), "/chat/completions");
        assert_eq!(external_sub_path("/v1/completions"), "/completions");
    }

    #[test]
    fn sub_path_never_double_prefixes_v1() {
        // The whole point of the strip: base_url carries `/v1`, so the sub-path must not.
        for p in [
            "/endpoints/v1/completions",
            "/v1/completions",
            "/endpoints/v1/chat/completions",
        ] {
            assert!(
                !external_sub_path(p).starts_with("/v1"),
                "{p} would produce a doubled /v1/v1 upstream"
            );
        }
    }

    // ---- proxy_to_external: outbound header hygiene + status pass-through ----
    //
    // These validate the billing-critical serving contract at the proxy seam. The
    // 2xx-only *metering* decision lives in `shared_endpoint_dispatch` (it gates on
    // `response.status().is_success()`), but that gate is only meaningful if the proxy
    // faithfully passes the provider's real status through — which is what these test.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn test_endpoint(base_url: String) -> ExternalEndpoint {
        ExternalEndpoint {
            slug: "m".to_string(),
            base_url,
            upstream_model: "u".to_string(),
            provider_api_key: "sk-provider-secret".to_string(),
            published: true,
            max_concurrency: -1,
            last_published_by: None,
            metrics_url: None,
            backends: None,
        }
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder().build().unwrap()
    }

    const FAST_TIMEOUTS: ExternalTimeouts = ExternalTimeouts {
        response_header_secs: 5,
        idle_secs: 5,
    };

    /// Read a full HTTP/1.1 request (request line + headers + body) from the socket,
    /// honoring Content-Length so the body is captured even if it arrives in a
    /// separate packet from the headers.
    async fn read_full_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let headers_end = buf
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4);
            if let Some(hend) = headers_end {
                let head = String::from_utf8_lossy(&buf[..hend]).to_lowercase();
                let want_body = head
                    .split("content-length:")
                    .nth(1)
                    .and_then(|s| s.split("\r\n").next())
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= hend + want_body {
                    break;
                }
            }
            match tokio::time::timeout(std::time::Duration::from_secs(2), socket.read(&mut tmp)).await
            {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => buf.extend_from_slice(&tmp[..n]),
                Ok(Err(_)) => break,
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    /// Spawn a one-shot server that captures the inbound request and replies with the
    /// given status line + body. Returns the port and a receiver for the raw request.
    async fn spawn_capture_server(
        status_line: &'static str,
        resp_body: &'static str,
    ) -> (u16, tokio::sync::oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let req = read_full_request(&mut socket).await;
            let http_resp = format!(
                "{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status_line,
                resp_body.len(),
                resp_body
            );
            let _ = socket.write_all(http_resp.as_bytes()).await;
            let _ = socket.flush().await;
            let _ = tx.send(req);
        });
        (port, rx)
    }

    async fn collect_body(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[tokio::test]
    async fn proxy_injects_provider_bearer_forwards_body_and_passes_2xx() {
        let (port, rx) = spawn_capture_server("HTTP/1.1 200 OK", "{\"usage\":{}}").await;
        let ext = test_endpoint(format!("http://127.0.0.1:{}/v1", port));
        let body = b"{\"model\":\"u\",\"messages\":[]}".to_vec();

        let resp = proxy_to_external(&test_client(), &ext.base_url, &ext.provider_api_key, "/chat/completions", body, FAST_TIMEOUTS).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "2xx must pass through");
        assert_eq!(collect_body(resp).await, "{\"usage\":{}}");

        let req = rx.await.unwrap();
        let lower = req.to_lowercase();
        // The gateway injects ONLY the provider key as Authorization.
        assert!(
            lower.contains("authorization: bearer sk-provider-secret"),
            "provider bearer must be injected; got: {req}"
        );
        // proxy_to_external never receives the caller's headers, so the InferX API key
        // (ix_...) can never leak to the provider — assert it structurally.
        assert!(!lower.contains("ix_"), "caller InferX key must never appear");
        // Path preserved and body forwarded verbatim.
        assert!(req.starts_with("POST /v1/chat/completions"), "got: {req}");
        assert!(req.contains("\"model\":\"u\""), "body must be forwarded");
    }

    #[tokio::test]
    async fn proxy_composes_legacy_completions_onto_base_url() {
        // End-to-end for `/endpoints/v1/completions`: the derived sub-path must land on
        // the provider as `/v1/completions` (base_url's `/v1` + `/completions`), with the
        // legacy `prompt` body forwarded verbatim rather than a chat `messages` array.
        let (port, rx) = spawn_capture_server("HTTP/1.1 200 OK", "{\"usage\":{}}").await;
        let ext = test_endpoint(format!("http://127.0.0.1:{}/v1", port));
        let sub_path = external_sub_path("/endpoints/v1/completions");
        let body = b"{\"model\":\"u\",\"prompt\":\"hi\"}".to_vec();

        let resp = proxy_to_external(&test_client(), &ext.base_url, &ext.provider_api_key, &sub_path, body, FAST_TIMEOUTS).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let req = rx.await.unwrap();
        assert!(
            req.starts_with("POST /v1/completions"),
            "legacy route must reach the provider as /v1/completions; got: {req}"
        );
        assert!(req.contains("\"prompt\":\"hi\""), "body must be forwarded");
    }

    #[tokio::test]
    async fn proxy_passes_provider_429_through_unchanged() {
        let (port, _rx) =
            spawn_capture_server("HTTP/1.1 429 Too Many Requests", "{\"error\":\"rate\"}").await;
        let ext = test_endpoint(format!("http://127.0.0.1:{}/v1", port));

        let resp = proxy_to_external(&test_client(), &ext.base_url, &ext.provider_api_key, "/chat/completions", b"{}".to_vec(), FAST_TIMEOUTS).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(collect_body(resp).await, "{\"error\":\"rate\"}");
    }

    #[tokio::test]
    async fn proxy_maps_connection_refused_to_connect_error() {
        let ext = test_endpoint("http://127.0.0.1:1/v1".to_string());
        let result = proxy_to_external(&test_client(), &ext.base_url, &ext.provider_api_key, "/chat/completions", b"{}".to_vec(), FAST_TIMEOUTS).await;
        assert!(result.is_err(), "connect failure must be Err (safe to retry)");
        let (code, _) = result.unwrap_err();
        assert_eq!(code, StatusCode::BAD_GATEWAY);
    }
}
