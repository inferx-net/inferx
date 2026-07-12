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
}

#[derive(Deserialize)]
pub struct ExternalEndpointUpdateRequest {
    pub base_url: String,
    pub upstream_model: String,
    /// Absent/null = keep the existing key; a non-empty value rotates it.
    #[serde(default)]
    pub provider_api_key: Option<String>,
}

/// Read-side view. Never carries `provider_api_key`.
#[derive(Serialize)]
pub struct ExternalEndpointView {
    pub slug: String,
    pub base_url: String,
    pub upstream_model: String,
    pub published: bool,
    pub has_api_key: bool,
    pub last_published_by: Option<String>,
}

impl From<&ExternalEndpoint> for ExternalEndpointView {
    fn from(e: &ExternalEndpoint) -> Self {
        Self {
            slug: e.slug.clone(),
            base_url: e.base_url.clone(),
            upstream_model: e.upstream_model.clone(),
            published: e.published,
            has_api_key: !e.provider_api_key.trim().is_empty(),
            last_published_by: e.last_published_by.clone(),
        }
    }
}

fn validate_base_url(base_url: &str) -> Result<()> {
    let url = url::Url::parse(base_url)
        .map_err(|_| Error::CommonError(format!("invalid base_url: {}", base_url)))?;
    if url.scheme() != "https" {
        return Err(Error::CommonError("base_url must be an https URL".to_string()));
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
        validate_base_url(base_url)?;

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
            .Create(slug, base_url, upstream_model, req.provider_api_key.trim())
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
        validate_base_url(base_url)?;

        // A blank/absent key means "keep existing"; only a non-empty value rotates it.
        let new_key = req
            .provider_api_key
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        let ep = self
            .externalEndpointMgr
            .Update(slug, base_url, upstream_model, new_key)
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
            last_published_by: None,
        };
        let view = ExternalEndpointView::from(&ep);
        assert!(view.has_api_key);
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("sk-super-secret"));
        assert!(!json.contains("provider_api_key"));
    }

    #[test]
    fn base_url_must_be_https() {
        assert!(validate_base_url("https://api.provider.com/v1").is_ok());
        assert!(validate_base_url("http://api.provider.com/v1").is_err());
        assert!(validate_base_url("not-a-url").is_err());
    }
}
