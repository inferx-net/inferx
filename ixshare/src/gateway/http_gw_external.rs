// Admin CRUD for external endpoints. Gated by EnsurePlatformEndpointAdmin, all
// writes go through the write-through manager. `provider_api_key` is write-only.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
    Extension, Json,
};
use hyper::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};

use crate::common::*;
use crate::gateway::auth_layer::AccessToken;
use crate::gateway::external_endpoint::is_valid_slug;
use crate::gateway::http_gateway::HttpGateway;
use crate::gateway::secret::ExternalEndpoint;

const PLATFORM_TENANT: &str = "inferx";
const PLATFORM_SHARED_NAMESPACE: &str = "endpoint";

#[derive(Deserialize)]
pub struct ExternalEndpointCreateRequest {
    pub slug: String,
    pub base_url: String,
    pub upstream_model: String,
    pub provider_api_key: String,
    /// Per-endpoint in-flight cap. Absent => `-1` (unlimited). Note the DB
    /// default never applies here: the value is always bound on insert, so the
    /// serde default must itself be `-1` — a plain `#[serde(default)]` would
    /// yield `0`, which is reject-all, not unlimited.
    #[serde(default = "unlimited_concurrency")]
    pub max_concurrency: i32,
    /// Opt-in to the dynamic ceiling controller. Absent = `None` = static.
    #[serde(default)]
    pub metrics_url: Option<String>,
}

/// Serde default for absent `max_concurrency`: `-1` = unlimited.
fn unlimited_concurrency() -> i32 {
    -1
}

#[derive(Deserialize)]
pub struct ExternalEndpointUpdateRequest {
    pub base_url: String,
    pub upstream_model: String,
    /// Absent/null = keep the existing key; a non-empty value rotates it.
    #[serde(default)]
    pub provider_api_key: Option<String>,
    /// Per-endpoint in-flight cap (`-1` = unlimited, `0` = reject all). Required
    /// & authoritative on update — sent unconditionally like
    /// `base_url`/`upstream_model`.
    pub max_concurrency: i32,
    /// Sent unconditionally like `max_concurrency`; `null` clears back to static.
    pub metrics_url: Option<String>,
}

/// Read-side view. Never carries `provider_api_key`.
#[derive(Serialize)]
pub struct ExternalEndpointView {
    pub slug: String,
    pub base_url: String,
    pub upstream_model: String,
    pub published: bool,
    pub has_api_key: bool,
    pub max_concurrency: i32,
    pub last_published_by: Option<String>,
    pub metrics_url: Option<String>,
}

impl From<&ExternalEndpoint> for ExternalEndpointView {
    fn from(e: &ExternalEndpoint) -> Self {
        Self {
            slug: e.slug.clone(),
            base_url: e.base_url.clone(),
            upstream_model: e.upstream_model.clone(),
            published: e.published,
            has_api_key: !e.provider_api_key.trim().is_empty(),
            max_concurrency: e.max_concurrency,
            last_published_by: e.last_published_by.clone(),
            metrics_url: e.metrics_url.clone(),
        }
    }
}

/// `http` is permitted so an external endpoint can target an in-cluster provider
/// (`http://svc:8000/v1`), where there is no public network hop to protect. Setting a
/// base_url is InferX-admin-only, so a plaintext scheme is a deliberate operator choice
/// — note that on a public `http` URL the provider API key is sent in the clear.
/// The parse itself is kept: `proxy_to_external` concatenates `base_url` with the
/// sub-path, so an unparseable value would only surface as a broken upstream request.
fn validate_base_url(base_url: &str) -> Result<()> {
    url::Url::parse(base_url)
        .map_err(|_| Error::CommonError(format!("invalid base_url: {}", base_url)))?;
    Ok(())
}

/// `metrics_url` opts a slug into the dynamic ceiling controller (contract 1 of
/// the dynamic-ceiling doc). It must be an `http`/`https` URL, and may only be
/// set when `max_concurrency > 0` — the controller needs a finite `N` to bound
/// the live ceiling within `[1, N]`.
fn validate_metrics_url(metrics_url: Option<&str>, max_concurrency: i32) -> Result<()> {
    let Some(metrics_url) = metrics_url else {
        return Ok(());
    };
    let parsed = url::Url::parse(metrics_url)
        .map_err(|_| Error::CommonError(format!("invalid metrics_url: {}", metrics_url)))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(Error::CommonError(format!(
            "metrics_url must be http or https: {}",
            metrics_url
        )));
    }
    if max_concurrency <= 0 {
        return Err(Error::CommonError(
            "metrics_url may only be set when max_concurrency > 0".to_string(),
        ));
    }
    Ok(())
}

impl HttpGateway {
    pub fn ExternalEndpointList(
        &self,
        token: &Arc<AccessToken>,
    ) -> Result<Vec<ExternalEndpointView>> {
        self.EnsurePlatformEndpointAdmin(token)?;
        Ok(self
            .externalEndpointMgr
            .List()
            .iter()
            .map(ExternalEndpointView::from)
            .collect())
    }

    pub fn ExternalEndpointGet(
        &self,
        token: &Arc<AccessToken>,
        slug: &str,
    ) -> Result<ExternalEndpointView> {
        self.EnsurePlatformEndpointAdmin(token)?;
        self.externalEndpointMgr
            .Get(slug)
            .as_ref()
            .map(ExternalEndpointView::from)
            .ok_or_else(|| Error::NotExist(format!("external endpoint {} not found", slug)))
    }

    pub async fn ExternalEndpointCreate(
        &self,
        token: &Arc<AccessToken>,
        req: ExternalEndpointCreateRequest,
    ) -> Result<ExternalEndpointView> {
        self.EnsurePlatformEndpointAdmin(token)?;

        let slug = req.slug.trim();
        let upstream_model = req.upstream_model.trim();
        let base_url = req.base_url.trim();
        if !is_valid_slug(slug) {
            return Err(Error::CommonError(
                "invalid slug (allowed: alphanumeric and -_.)".to_string(),
            ));
        }
        if upstream_model.is_empty() {
            return Err(Error::CommonError("upstream_model is required".to_string()));
        }
        if req.provider_api_key.trim().is_empty() {
            return Err(Error::CommonError("provider_api_key is required".to_string()));
        }
        if req.max_concurrency < -1 {
            return Err(Error::CommonError(
                "max_concurrency must be >= -1 (-1 = unlimited, 0 = reject all)".to_string(),
            ));
        }
        validate_base_url(base_url)?;
        let metrics_url = req.metrics_url.as_deref().map(str::trim).filter(|s| !s.is_empty());
        validate_metrics_url(metrics_url, req.max_concurrency)?;

        // Single-kind invariant: a slug is external xor a self-hosted func.
        if self
            .objRepo
            .GetFunc(PLATFORM_TENANT, PLATFORM_SHARED_NAMESPACE, slug)
            .is_ok()
        {
            return Err(Error::CommonError(format!(
                "slug {} is already a self-hosted endpoint",
                slug
            )));
        }
        if self.externalEndpointMgr.Contains(slug) {
            return Err(Error::CommonError(format!(
                "external endpoint {} already exists",
                slug
            )));
        }

        let ep = self
            .externalEndpointMgr
            .Create(
                slug,
                base_url,
                upstream_model,
                req.provider_api_key.trim(),
                req.max_concurrency,
                metrics_url,
            )
            .await?;
        Ok(ExternalEndpointView::from(&ep))
    }

    pub async fn ExternalEndpointUpdate(
        &self,
        token: &Arc<AccessToken>,
        slug: &str,
        req: ExternalEndpointUpdateRequest,
    ) -> Result<ExternalEndpointView> {
        self.EnsurePlatformEndpointAdmin(token)?;

        let upstream_model = req.upstream_model.trim();
        let base_url = req.base_url.trim();
        if upstream_model.is_empty() {
            return Err(Error::CommonError("upstream_model is required".to_string()));
        }
        if req.max_concurrency < -1 {
            return Err(Error::CommonError(
                "max_concurrency must be >= -1 (-1 = unlimited, 0 = reject all)".to_string(),
            ));
        }
        validate_base_url(base_url)?;
        let metrics_url = req.metrics_url.as_deref().map(str::trim).filter(|s| !s.is_empty());
        validate_metrics_url(metrics_url, req.max_concurrency)?;

        // A blank/absent key means "keep existing"; only a non-empty value rotates it.
        let new_key = req
            .provider_api_key
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        let ep = self
            .externalEndpointMgr
            .Update(slug, base_url, upstream_model, new_key, req.max_concurrency, metrics_url)
            .await?;
        Ok(ExternalEndpointView::from(&ep))
    }

    /// Full teardown across all slug-keyed state. If the slug is still listed on
    /// OpenRouter, refuse and require the two-step offline -> unlist first (preserves
    /// the drain window). Otherwise: unpublish, drop the catalog row, drop the
    /// serving row. TokenRate history is left intact.
    pub async fn ExternalEndpointDelete(&self, token: &Arc<AccessToken>, slug: &str) -> Result<()> {
        self.EnsurePlatformEndpointAdmin(token)?;

        if !self.externalEndpointMgr.Contains(slug) {
            return Err(Error::NotExist(format!(
                "external endpoint {} not found",
                slug
            )));
        }

        if let Some(row) = self.sqlSecret.GetEndpointForListing(slug).await? {
            if row.or_listed {
                return Err(Error::CommonError(format!(
                    "endpoint {} is listed on OpenRouter; take it offline and unlist before deleting",
                    slug
                )));
            }
        }

        self.externalEndpointMgr
            .SetPublished(slug, false, &token.username)
            .await?;
        self.sqlSecret.DeleteEndpointRow(slug).await?;
        self.externalEndpointMgr.Delete(slug).await?;
        Ok(())
    }
}

fn error_response(err: Error) -> Response {
    let status = match &err {
        Error::NoPermission => StatusCode::UNAUTHORIZED,
        Error::NotExist(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_REQUEST,
    };
    Response::builder()
        .status(status)
        .body(Body::from(format!("service failure {:?}", err)))
        .unwrap()
}

fn json_ok<T: Serialize>(value: &T) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(value).unwrap_or_default()))
        .unwrap()
}

pub async fn ListExternalEndpoints(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
) -> std::result::Result<Response, StatusCode> {
    match gw.ExternalEndpointList(&token) {
        Ok(list) => Ok(json_ok(&list)),
        Err(e) => Ok(error_response(e)),
    }
}

pub async fn CreateExternalEndpoint(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Json(req): Json<ExternalEndpointCreateRequest>,
) -> std::result::Result<Response, StatusCode> {
    match gw.ExternalEndpointCreate(&token, req).await {
        Ok(view) => Ok(json_ok(&view)),
        Err(e) => Ok(error_response(e)),
    }
}

pub async fn GetExternalEndpoint(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
) -> std::result::Result<Response, StatusCode> {
    match gw.ExternalEndpointGet(&token, &slug) {
        Ok(view) => Ok(json_ok(&view)),
        Err(e) => Ok(error_response(e)),
    }
}

pub async fn UpdateExternalEndpoint(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
    Json(req): Json<ExternalEndpointUpdateRequest>,
) -> std::result::Result<Response, StatusCode> {
    match gw.ExternalEndpointUpdate(&token, &slug, req).await {
        Ok(view) => Ok(json_ok(&view)),
        Err(e) => Ok(error_response(e)),
    }
}

pub async fn DeleteExternalEndpoint(
    Extension(token): Extension<Arc<AccessToken>>,
    State(gw): State<HttpGateway>,
    Path(slug): Path<String>,
) -> std::result::Result<Response, StatusCode> {
    match gw.ExternalEndpointDelete(&token, &slug).await {
        Ok(()) => Ok(json_ok(&serde_json::json!({ "deleted": true }))),
        Err(e) => Ok(error_response(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_never_exposes_key() {
        let ep = ExternalEndpoint {
            slug: "m".to_string(),
            base_url: "https://api.provider.com/v1".to_string(),
            upstream_model: "u".to_string(),
            provider_api_key: "sk-super-secret".to_string(),
            published: true,
            max_concurrency: -1,
            last_published_by: None,
            metrics_url: None,
        };
        let view = ExternalEndpointView::from(&ep);
        assert!(view.has_api_key);
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("sk-super-secret"));
        assert!(!json.contains("provider_api_key"));
    }

    #[test]
    fn base_url_allows_http_for_in_cluster_providers() {
        assert!(validate_base_url("https://api.provider.com/v1").is_ok());
        // In-cluster providers have no public hop; plaintext is an admin's choice to make.
        assert!(validate_base_url("http://svc:8000/v1").is_ok());
        assert!(validate_base_url("http://vllm.default.svc.cluster.local:8000/v1").is_ok());
        assert!(validate_base_url("http://127.0.0.1:8000/v1").is_ok());
        // Still rejected: not a URL at all, which would only break at proxy time.
        assert!(validate_base_url("not-a-url").is_err());
    }
}
