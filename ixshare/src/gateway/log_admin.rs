use std::result::Result as SResult;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Path;
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::Response;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};

use crate::gateway::auth_layer::AccessToken;
use crate::print::{
    disable_verbose_category, enable_verbose_category, mask_to_env_names, replace_verbose_log_mask,
    verbose_category_by_env_name, verbose_log_mask,
};

#[derive(Debug, Deserialize)]
pub struct VerboseCategoriesRequest {
    pub categories: Vec<String>,
}

#[derive(Debug, Serialize)]
struct VerboseCategoriesResponse {
    categories: Vec<String>,
    mask: u64,
}

#[derive(Debug, Serialize)]
struct VerboseCategoriesErrorResponse {
    error: String,
    categories: Vec<String>,
}

fn verbose_categories_response(mask: u64) -> Response {
    let body = serde_json::to_string(&VerboseCategoriesResponse {
        categories: mask_to_env_names(mask)
            .into_iter()
            .map(str::to_owned)
            .collect(),
        mask,
    })
    .unwrap();

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn verbose_categories_error_response(
    status: StatusCode,
    error: impl Into<String>,
    categories: Vec<String>,
) -> Response {
    let body = serde_json::to_string(&VerboseCategoriesErrorResponse {
        error: error.into(),
        categories,
    })
    .unwrap();

    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

pub fn verbose_categories_mask_from_request(
    categories: &[String],
) -> std::result::Result<u64, Vec<String>> {
    let mut mask = 0u64;
    let mut unknown = Vec::new();

    for category in categories {
        let normalized = category.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            if !unknown.iter().any(|value: &String| value.is_empty()) {
                unknown.push(String::new());
            }
            continue;
        }

        match verbose_category_by_env_name(category) {
            Some(category) => mask |= category.mask,
            None => {
                if !unknown.iter().any(|value| value == &normalized) {
                    unknown.push(normalized);
                }
            }
        }
    }

    if unknown.is_empty() {
        Ok(mask)
    } else {
        Err(unknown)
    }
}

fn require_inferx_admin(token: &AccessToken) -> std::result::Result<(), Response> {
    if token.IsInferxAdmin() {
        Ok(())
    } else {
        Err(verbose_categories_error_response(
            StatusCode::FORBIDDEN,
            "service failure NoPermission",
            Vec::new(),
        ))
    }
}

fn log_verbose_mask_change(action: &str, old_mask: u64, new_mask: u64) {
    info!(
        "verbose log mask changed action={} old_categories={:?} new_categories={:?} old_mask={} new_mask={}",
        action,
        mask_to_env_names(old_mask),
        mask_to_env_names(new_mask),
        old_mask,
        new_mask
    );
}

pub async fn GetVerboseCategories(
    Extension(token): Extension<Arc<AccessToken>>,
) -> SResult<Response, StatusCode> {
    if let Err(resp) = require_inferx_admin(&token) {
        return Ok(resp);
    }

    Ok(verbose_categories_response(verbose_log_mask()))
}

pub async fn PutVerboseCategories(
    Extension(token): Extension<Arc<AccessToken>>,
    Json(request): Json<VerboseCategoriesRequest>,
) -> SResult<Response, StatusCode> {
    if let Err(resp) = require_inferx_admin(&token) {
        return Ok(resp);
    }

    let new_mask = match verbose_categories_mask_from_request(&request.categories) {
        Ok(mask) => mask,
        Err(unknown) => {
            error!(
                "rejected verbose category update unknown_categories={:?} requested={:?}",
                unknown, request.categories
            );
            return Ok(verbose_categories_error_response(
                StatusCode::BAD_REQUEST,
                "unknown verbose categories",
                unknown,
            ));
        }
    };

    let old_mask = replace_verbose_log_mask(new_mask);
    log_verbose_mask_change("replace", old_mask, new_mask);
    Ok(verbose_categories_response(new_mask))
}

pub async fn EnableVerboseCategory(
    Extension(token): Extension<Arc<AccessToken>>,
    Path(category): Path<String>,
) -> SResult<Response, StatusCode> {
    if let Err(resp) = require_inferx_admin(&token) {
        return Ok(resp);
    }

    let category = match verbose_category_by_env_name(&category) {
        Some(category) => category,
        None => {
            let unknown = category.trim().to_ascii_lowercase();
            error!(
                "rejected verbose category enable unknown_category={}",
                unknown
            );
            return Ok(verbose_categories_error_response(
                StatusCode::BAD_REQUEST,
                "unknown verbose categories",
                vec![unknown],
            ));
        }
    };

    let old_mask = enable_verbose_category(category);
    let new_mask = old_mask | category.mask;
    log_verbose_mask_change("enable", old_mask, new_mask);
    Ok(verbose_categories_response(new_mask))
}

pub async fn DisableVerboseCategory(
    Extension(token): Extension<Arc<AccessToken>>,
    Path(category): Path<String>,
) -> SResult<Response, StatusCode> {
    if let Err(resp) = require_inferx_admin(&token) {
        return Ok(resp);
    }

    let category = match verbose_category_by_env_name(&category) {
        Some(category) => category,
        None => {
            let unknown = category.trim().to_ascii_lowercase();
            error!(
                "rejected verbose category disable unknown_category={}",
                unknown
            );
            return Ok(verbose_categories_error_response(
                StatusCode::BAD_REQUEST,
                "unknown verbose categories",
                vec![unknown],
            ));
        }
    };

    let old_mask = disable_verbose_category(category);
    let new_mask = old_mask & !category.mask;
    log_verbose_mask_change("disable", old_mask, new_mask);
    Ok(verbose_categories_response(new_mask))
}
