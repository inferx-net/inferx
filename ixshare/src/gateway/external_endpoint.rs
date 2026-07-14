// External OpenAI-compatible endpoints: a Postgres table mirrored in full into a
// gateway BTreeMap, kept exact by write-through on every admin write. Serving
// reads never touch the DB. Coherent for a single gateway only.

use std::collections::BTreeMap;
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

/// Upstream liveness deadlines for the outbound proxy (seconds). Passed in by the
/// caller — global gateway-config defaults in production, explicit values in tests —
/// so `proxy_to_external` does not depend on the global config being initialized.
/// No total-request-time cap by design, so long streaming generations never sever.
#[derive(Debug, Clone, Copy)]
pub struct ExternalTimeouts {
    pub connect_secs: u64,
    pub response_header_secs: u64,
    pub idle_secs: u64,
}

impl ExternalTimeouts {
    pub fn from_gateway_config() -> Self {
        Self {
            connect_secs: GATEWAY_CONFIG.externalConnectTimeoutSecs,
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

#[derive(Debug, Clone)]
pub struct ExternalEndpointMgr {
    sql: SqlSecret,
    map: Arc<RwLock<BTreeMap<String, ExternalEndpoint>>>,
}

impl ExternalEndpointMgr {
    /// Hydrate the full mirror from Postgres at gateway startup.
    pub async fn New(sql: SqlSecret) -> Result<Self> {
        let rows = sql.LoadExternalEndpoints().await?;
        let mut map = BTreeMap::new();
        for row in rows {
            map.insert(row.slug.clone(), row);
        }
        Ok(Self {
            sql,
            map: Arc::new(RwLock::new(map)),
        })
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

    fn upsert_mirror(&self, ep: ExternalEndpoint) {
        self.map.write().unwrap().insert(ep.slug.clone(), ep);
    }

    pub async fn Create(
        &self,
        slug: &str,
        base_url: &str,
        upstream_model: &str,
        provider_api_key: &str,
    ) -> Result<ExternalEndpoint> {
        let ep = self
            .sql
            .InsertExternalEndpoint(slug, base_url, upstream_model, provider_api_key)
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
    ) -> Result<ExternalEndpoint> {
        let ep = self
            .sql
            .UpdateExternalEndpoint(slug, base_url, upstream_model, provider_api_key)
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

    pub async fn Delete(&self, slug: &str) -> Result<()> {
        self.sql.DeleteExternalEndpoint(slug).await?;
        self.map.write().unwrap().remove(slug);
        Ok(())
    }
}

fn external_gateway_error(code: StatusCode, msg: String) -> Response {
    Response::builder().status(code).body(Body::from(msg)).unwrap()
}

/// Stream a call to the provider with connect/response-header/idle deadlines (not reqwest's
/// total-duration `.timeout()`) and adapt the reqwest response into an axum one.
/// Fresh outbound headers only: the caller's Authorization is never forwarded.
/// `sub_path` has the `/v1` root stripped (base_url carries it).
pub async fn proxy_to_external(
    ext: &ExternalEndpoint,
    sub_path: &str,
    body_bytes: Vec<u8>,
    timeouts: ExternalTimeouts,
) -> Response {
    let url = format!("{}{}", ext.base_url.trim_end_matches('/'), sub_path);
    let client = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(timeouts.connect_secs))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return external_gateway_error(
                StatusCode::BAD_GATEWAY,
                format!("service failure: external client build error: {e}"),
            )
        }
    };

    let send = client
        .post(&url)
        .header("content-type", "application/json")
        .bearer_auth(&ext.provider_api_key)
        .body(body_bytes)
        .send();

    let resp = match tokio::time::timeout(
        std::time::Duration::from_secs(timeouts.response_header_secs),
        send,
    )
    .await
    {
        // Response headers never arrived.
        Err(_) => {
            return external_gateway_error(
                StatusCode::GATEWAY_TIMEOUT,
                "service failure: upstream response header timeout".to_string(),
            )
        }
        // Transport failure, no HTTP response: connect-timeout -> 504, refused/DNS -> 502.
        Ok(Err(e)) => {
            let code = if e.is_timeout() {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
            return external_gateway_error(
                code,
                format!("service failure: upstream transport error: {e}"),
            );
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

    builder
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap()
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
            last_published_by: None,
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
            last_published_by: None,
        }
    }

    const FAST_TIMEOUTS: ExternalTimeouts = ExternalTimeouts {
        connect_secs: 5,
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

        let resp = proxy_to_external(&ext, "/chat/completions", body, FAST_TIMEOUTS).await;
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

        let resp = proxy_to_external(&ext, &sub_path, body, FAST_TIMEOUTS).await;
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

        let resp = proxy_to_external(&ext, "/chat/completions", b"{}".to_vec(), FAST_TIMEOUTS).await;
        // Real provider status is surfaced so the client can back off (never remapped).
        // The dispatch 2xx gate then skips metering for this non-2xx.
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(collect_body(resp).await, "{\"error\":\"rate\"}");
    }

    #[tokio::test]
    async fn proxy_maps_connection_refused_to_502() {
        // Nothing is listening on this port → connection refused (no HTTP response).
        let ext = test_endpoint("http://127.0.0.1:1/v1".to_string());
        let resp = proxy_to_external(&ext, "/chat/completions", b"{}".to_vec(), FAST_TIMEOUTS).await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}
