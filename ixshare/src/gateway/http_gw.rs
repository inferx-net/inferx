use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use inferxlib::{
    data_obj::DataObject,
    obj_mgr::{
        func_mgr::{FuncStatus, Function},
        funcstatus_mgr::{FunctionStatus, FunctionStatusDef},
        funcpolicy_mgr::FuncPolicy,
        funcsnapshot_mgr::FuncSnapshot,
        namespace_mgr::{Namespace, NamespaceObject},
        pod_mgr::FuncPod,
        tenant_mgr::{Tenant, TenantObject, SYSTEM_NAMESPACE, SYSTEM_TENANT},
    },
};
use rand::Rng;

use serde_json::Value;

use crate::{
    audit::{PodAuditLog, PodFailLog, SnapshotScheduleAudit},
    common::*,
    metastore::selection_predicate::ListOption,
    node_config::KB_DIR,
};

use super::{
    auth_layer::{
        AccessToken, ApikeyCreateRequest, GetTokenCache, ObjectType, PermissionType, UserRole,
    },
    gw_obj_repo::{FuncBrief, FuncDetail},
    http_gateway::{HttpGateway, GATEWAY_CONFIG},
    secret::{EndpointMetadata, EndpointOpenRouterMetadata, ListedEndpoint},
};

const ONBOARD_TENANT_PREFIX: &str = "tn-";
const ONBOARD_TENANT_SUFFIX_LEN: usize = 10;
const ONBOARD_DEFAULT_NAMESPACE: &str = "default";
const ONBOARD_MAX_ATTEMPTS: usize = 3;
const ONBOARD_APIKEY_PREFIX: &str = "quickstart-inference";
const ONBOARD_INITIAL_CREDIT_NOTE: &str = "Initial onboarding credit";
const ONBOARD_INITIAL_CREDIT_ADDED_BY: &str = "system-onboard";
const ONBOARD_INITIAL_CREDIT_PAYMENT_REF_PREFIX: &str = "onboard-initial-credit";
const PLATFORM_TENANT: &str = "inferx";
const PLATFORM_SHARED_NAMESPACE: &str = "endpoint";
const KB_FILE_NAME: &str = "kb.data";

fn kb_func_root_path(tenant: &str, namespace: &str, name: &str) -> PathBuf {
    Path::new(KB_DIR).join(format!("{}.{}.{}", tenant, namespace, name))
}

fn kb_version_dir_path(tenant: &str, namespace: &str, name: &str, version: i64) -> PathBuf {
    kb_func_root_path(tenant, namespace, name).join(version.to_string())
}

fn kb_stage_file_path(tenant: &str, namespace: &str, name: &str) -> PathBuf {
    kb_func_root_path(tenant, namespace, name).join(KB_FILE_NAME)
}

fn kb_version_file_path(tenant: &str, namespace: &str, name: &str, version: i64) -> PathBuf {
    kb_version_dir_path(tenant, namespace, name, version).join(KB_FILE_NAME)
}

impl HttpGateway {
    fn HasInferxTenantPolicyOverrides(policy: &crate::node_config::InferxTenantPolicy) -> bool {
        policy.quota_exempt.is_some()
            || policy.allowMemStandby.is_some()
            || policy.maxFuncCnt.is_some()
            || policy.maxReplica.is_some()
            || policy.maxGpu.is_some()
            || policy.maxStandby.is_some()
            || policy.maxQueueLen.is_some()
    }

    fn ApplyInferxTenantPolicy(
        tenant_obj: &mut Tenant,
        policy: &crate::node_config::InferxTenantPolicy,
    ) -> bool {
        let mut changed = false;

        if let Some(v) = policy.quota_exempt {
            if tenant_obj.object.spec.quota_exempt != v {
                tenant_obj.object.spec.quota_exempt = v;
                changed = true;
            }
        }
        if let Some(v) = policy.allowMemStandby {
            if tenant_obj.object.spec.resourceLimit.allocMemStandby != v {
                tenant_obj.object.spec.resourceLimit.allocMemStandby = v;
                changed = true;
            }
        }
        if let Some(v) = policy.maxFuncCnt {
            if tenant_obj.object.spec.resourceLimit.maxFuncCnt != v {
                tenant_obj.object.spec.resourceLimit.maxFuncCnt = v;
                changed = true;
            }
        }
        if let Some(v) = policy.maxReplica {
            if tenant_obj.object.spec.resourceLimit.maxReplica != v {
                tenant_obj.object.spec.resourceLimit.maxReplica = v;
                changed = true;
            }
        }
        if let Some(v) = policy.maxGpu {
            if tenant_obj.object.spec.resourceLimit.maxGpu != v {
                tenant_obj.object.spec.resourceLimit.maxGpu = v;
                changed = true;
            }
        }
        if let Some(v) = policy.maxStandby {
            if tenant_obj.object.spec.resourceLimit.maxStandby != v {
                tenant_obj.object.spec.resourceLimit.maxStandby = v;
                changed = true;
            }
        }
        if let Some(v) = policy.maxQueueLen {
            if tenant_obj.object.spec.resourceLimit.maxQueueLen != v {
                tenant_obj.object.spec.resourceLimit.maxQueueLen = v;
                changed = true;
            }
        }

        changed
    }

    fn ensure_kb_file_readable(path: &Path) -> Result<()> {
        std::fs::File::open(path).map_err(|e| {
            Error::CommonError(format!(
                "knowledgebase file {} is not readable: {:?}",
                path.display(),
                e
            ))
        })?;
        Ok(())
    }

    fn prepare_kb_stage_promotion(
        tenant: &str,
        namespace: &str,
        name: &str,
        version: i64,
    ) -> Result<(PathBuf, PathBuf, PathBuf)> {
        let stage_file = kb_stage_file_path(tenant, namespace, name);
        Self::ensure_kb_file_readable(&stage_file)?;

        let version_dir = kb_version_dir_path(tenant, namespace, name, version);
        std::fs::create_dir_all(&version_dir)?;
        let version_file = version_dir.join(KB_FILE_NAME);
        std::fs::rename(&stage_file, &version_file)?;

        Ok((stage_file, version_dir, version_file))
    }

    fn rollback_kb_stage_promotion(stage_file: &Path, version_dir: &Path, version_file: &Path) {
        if version_file.exists() {
            if let Some(parent) = stage_file.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    error!(
                        "failed to recreate KB staging parent {} during rollback: {:?}",
                        parent.display(),
                        e
                    );
                }
            }

            if let Err(e) = std::fs::rename(version_file, stage_file) {
                error!(
                    "failed to rollback KB file {} -> {}: {:?}",
                    version_file.display(),
                    stage_file.display(),
                    e
                );
            }
        }

        if let Err(e) = std::fs::remove_dir(version_dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                error!(
                    "failed to remove KB version dir {} during rollback: {:?}",
                    version_dir.display(),
                    e
                );
            }
        }
    }

    fn prepare_kb_version_copy(
        tenant: &str,
        namespace: &str,
        name: &str,
        old_version: i64,
        new_version: i64,
    ) -> Result<PathBuf> {
        let src_file = kb_version_file_path(tenant, namespace, name, old_version);
        Self::ensure_kb_file_readable(&src_file)?;

        let version_dir = kb_version_dir_path(tenant, namespace, name, new_version);
        std::fs::create_dir_all(&version_dir)?;
        let dst_file = version_dir.join(KB_FILE_NAME);
        std::fs::copy(&src_file, &dst_file)?;

        Ok(version_dir)
    }

    fn cleanup_kb_version_dir(version_dir: &Path) {
        if let Err(e) = std::fs::remove_dir_all(version_dir) {
            error!(
                "failed to remove KB version dir {}: {:?}",
                version_dir.display(),
                e
            );
        }
    }

    pub async fn SaveEndpointMetadata(
        &self,
        token: &Arc<AccessToken>,
        slug: &str,
        metadata: &EndpointMetadata,
    ) -> Result<i64> {
        self.EnsurePlatformEndpointAdmin(token)?;
        let func = self.GetPlatformEndpointFunc(slug).await?;
        self.sqlSecret
            .UpsertEndpointMetadata(slug, func.Version(), metadata)
            .await?;
        Ok(func.Version())
    }

    pub async fn PublishEndpoint(
        &self,
        token: &Arc<AccessToken>,
        slug: &str,
        metadata: &EndpointMetadata,
    ) -> Result<i64> {
        self.EnsurePlatformEndpointAdmin(token)?;

        let mut attempts = 0;
        loop {
            attempts += 1;
            let func = self.GetPlatformEndpointFunc(slug).await?;
            let mut status = self.GetPlatformEndpointStatus(slug).await?;

            if status.object.version != func.Version() {
                return Err(Error::CommonError(format!(
                    "funcstatus version {} does not match function version {} for endpoint {}",
                    status.object.version,
                    func.Version(),
                    slug
                )));
            }

            self.sqlSecret
                .PublishEndpoint(slug, func.Version(), metadata, &token.username)
                .await?;

            // Derive hugging_face_id from the func's `--model` (the actual weights the
            // pod loads) when the column is empty and the value has the `author/name`
            // HF shape (§ derive at publish). This makes the HF id present before the
            // OpenRouter dialog opens and ties it to the served weights. A local mount
            // path (no HF shape) is left for manual entry. Never overwrites a stored id.
            if let Some(model_path) = func.object.spec.ModelPath() {
                if looks_like_hf_repo_id(&model_path) {
                    self.sqlSecret
                        .SetEndpointHuggingFaceIdIfEmpty(slug, model_path.trim())
                        .await?;
                }
            }

            status.object.published = true;
            match self.client.Update(&status.DataObject(), status.revision).await {
                Ok(revision) => return Ok(revision),
                Err(e) if attempts < 3 && IsCasConflictError(&e) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn UnpublishEndpoint(
        &self,
        token: &Arc<AccessToken>,
        slug: &str,
    ) -> Result<i64> {
        self.EnsurePlatformEndpointAdmin(token)?;

        let mut attempts = 0;
        loop {
            attempts += 1;
            let mut status = self.GetPlatformEndpointStatus(slug).await?;

            status.object.published = false;
            match self.client.Update(&status.DataObject(), status.revision).await {
                Ok(revision) => return Ok(revision),
                Err(e) if attempts < 3 && IsCasConflictError(&e) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Persist the operator-authored OpenRouter listing metadata. This is the
    /// draft-save path for the admin "OpenRouter listing" section. It does NOT validate
    /// listing completeness (`ListOnOpenRouter` is that chokepoint), but it IS the
    /// single write gate for `openrouter_slug`: a *changed* slug is catalog-validated
    /// before it is persisted, so the column never holds an invalid
    /// value. Decoupled from publish — only the func must be deployed, not published.
    ///
    /// Returns the non-blocking slug warnings (HF-id mismatch / `:free` pin) so the
    /// caller can surface them and gate the auto-list on an explicit acknowledge.
    pub async fn SaveEndpointOpenRouterMetadata(
        &self,
        token: &Arc<AccessToken>,
        slug: &str,
        metadata: &EndpointOpenRouterMetadata,
    ) -> Result<Vec<String>> {
        self.EnsurePlatformEndpointAdmin(token)?;
        // The func must be deployed (proves it exists). This does NOT check `published`
        // — OpenRouter serving is decoupled from publish (§ Publish decoupling).
        let func = self.GetPlatformEndpointFunc(slug).await?;

        let existing = self.sqlSecret.GetEndpointForListing(slug).await?;

        // Resolve the slug to persist under the one-column invariant:
        //   - unchanged      → keep the stored value, no catalog fetch / re-validation
        //   - changed→value  → validate against the live catalog; store only if a real
        //                      catalog `id` (warnings, not rejection, for :free/mismatch)
        //   - changed→empty  → cleared; go back to auto (re-suggested on next dialog open)
        let stored_slug = existing
            .as_ref()
            .and_then(|e| e.openrouter_slug.clone())
            .filter(|s| !s.trim().is_empty());
        let submitted_slug = metadata
            .openrouter_slug
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let mut warnings: Vec<String> = Vec::new();
        let final_slug: Option<String> = if submitted_slug == stored_slug {
            stored_slug.clone()
        } else if let Some(sub) = &submitted_slug {
            // Catalog fetch happens ONLY here — when the slug actually changed.
            let validation = self
                .validate_openrouter_slug(sub, metadata.hugging_face_id.as_deref())
                .await?;
            warnings = validation.warnings;
            Some(sub.clone())
        } else {
            None
        };

        let mut to_store = metadata.clone();
        to_store.openrouter_slug = final_slug;

        // Guard the live catalog: while an endpoint is listed, a draft save must not
        // make it invalid. `/v1/models` emits the `or_listed` subset verbatim, so a
        // breaking save would silently degrade the live entry outside the
        // offline/delist lifecycle. Require the operator to take it offline / delist
        // before editing a required field on a live listing.
        if let Some(existing) = &existing {
            if existing.or_listed {
                let prospective = MergeListingMetadata(existing, &to_store);
                if let Err(e) = ValidateOpenRouterListing(&prospective) {
                    return Err(Error::CommonError(format!(
                        "endpoint {} is live on OpenRouter; this change would make it invalid ({:?}). Take it offline / delist before editing required fields.",
                        slug, e
                    )));
                }
            }
        }

        self.sqlSecret
            .SaveEndpointOpenRouterMetadata(slug, func.Version(), &to_store)
            .await?;
        Ok(warnings)
    }

    /// List a published endpoint on OpenRouter. The single validation
    /// chokepoint: confirms every OpenRouter-required field is present & well-formed
    /// (rejecting null required modalities and refusing to guess), resolves the
    /// canonical `openrouter_slug`, then flips `or_listed=true`.
    pub async fn ListOnOpenRouter(&self, token: &Arc<AccessToken>, slug: &str) -> Result<()> {
        self.EnsurePlatformEndpointAdmin(token)?;
        // Endpoint must be published first (the func must exist).
        self.GetPlatformEndpointFunc(slug).await?;

        let row = self
            .sqlSecret
            .GetEndpointForListing(slug)
            .await?
            .ok_or_else(|| {
                Error::NotExist(format!(
                    "endpoint {} has no listing metadata; fill the OpenRouter section first",
                    slug
                ))
            })?;

        // Validates every emitted-schema field, including the required `hugging_face_id`
        // (proposal rule 2: provenance + the auto-discovery join key). A manual slug
        // edit does NOT exempt the row from carrying an HF id.
        ValidateOpenRouterListing(&row)?;

        // Attach-or-fail (one-column model): List never recomputes the slug — it
        // emits the stored `openrouter_slug`, which was resolved at Save (a validated
        // admin edit) or at prefill (auto-suggest accepted into a previously-empty
        // column, then persisted by that Save). An empty slug fails closed; there is
        // no "list without openrouter.slug / new page" fallback.
        let openrouter_slug = row
            .openrouter_slug
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::CommonError(format!(
                    "endpoint {} has no openrouter_slug; resolve it in the OpenRouter section (\"Suggest from HF ID\", or type a catalog id) before listing",
                    slug
                ))
            })?;

        self.sqlSecret
            .SetEndpointListed(slug, Some(openrouter_slug), &token.username)
            .await?;
        Ok(())
    }

    /// Step 1 of graceful delist: take a listed endpoint offline by emitting
    /// `is_ready:false` (+ optional planned deprecation date). Still listed so
    /// OpenRouter drains.
    pub async fn TakeOfflineOnOpenRouter(
        &self,
        token: &Arc<AccessToken>,
        slug: &str,
        deprecation_date: Option<chrono::NaiveDate>,
    ) -> Result<()> {
        self.EnsurePlatformEndpointAdmin(token)?;
        self.sqlSecret
            .SetEndpointOpenRouterReady(slug, false, deprecation_date)
            .await?;
        Ok(())
    }

    /// Step 2 of graceful delist: drop the row from `/v1/models` after drain.
    /// Enforces the offline-first ordering — refuses to hard-remove a row that is
    /// still `is_ready`, since OpenRouter is silent on in-flight traffic when an
    /// entry just vanishes.
    pub async fn UnlistFromOpenRouter(&self, token: &Arc<AccessToken>, slug: &str) -> Result<()> {
        self.EnsurePlatformEndpointAdmin(token)?;

        let row = self
            .sqlSecret
            .GetEndpointForListing(slug)
            .await?
            .ok_or_else(|| Error::NotExist(format!("endpoint {} not found", slug)))?;
        if !row.or_listed {
            return Err(Error::CommonError(format!(
                "endpoint {} is not listed on OpenRouter",
                slug
            )));
        }
        if row.or_is_ready != Some(false) {
            return Err(Error::CommonError(format!(
                "endpoint {} must be taken offline (is_ready=false) and drained before delisting",
                slug
            )));
        }

        self.sqlSecret.SetEndpointUnlisted(slug).await?;
        Ok(())
    }

    /// Fetch the OpenRouter public catalog `data` array (bounded timeout so a hung
    /// fetch can't block the admin op forever). Shared by the suggest (auto) and
    /// validate (admin-edit) paths.
    async fn fetch_openrouter_catalog(&self) -> Result<Vec<Value>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| Error::CommonError(format!("failed to build HTTP client: {e}")))?;
        let catalog: Value = client
            .get("https://openrouter.ai/api/v1/models")
            .send()
            .await
            .map_err(|e| Error::CommonError(format!("failed to fetch OpenRouter catalog: {e}")))?
            .json()
            .await
            .map_err(|e| Error::CommonError(format!("invalid OpenRouter catalog JSON: {e}")))?;

        let data = catalog
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| Error::CommonError("OpenRouter catalog missing `data` array".into()))?;
        Ok(data.clone())
    }

    /// Suggestion/auto path. Join the supplied `hf_id` — taken from the
    /// request/form, NOT the DB row, because on a first listing the row has no HF id
    /// yet (the ordering fix) — against the OpenRouter catalog and apply the
    /// paid-over-`:free` tie-break. **Best-effort:** returns `None` on no
    /// match / unresolved ambiguity / fetch failure so the editor never errors; the
    /// submit-time `validate_openrouter_slug` is the gate, not the suggestion. Never
    /// suggests a `:free` slug.
    pub async fn suggest_openrouter_slug(&self, hf_id: &str) -> Option<String> {
        let hf_id = hf_id.trim();
        if hf_id.is_empty() {
            return None;
        }
        let data = self.fetch_openrouter_catalog().await.ok()?;
        let matches = JoinCatalogOnHfId(hf_id, &data);
        // No-match / ambiguity → None (best-effort: never errors the dialog).
        ResolveSlugFromMatches(hf_id, matches).ok().flatten()
    }

    /// Admin-edit path (§ Validation Rules). The hard gate before a changed slug is
    /// persisted: `slug` must exactly match some OpenRouter catalog entry's **`id`**
    /// field — a nonexistent slug (including a string that exists only as a
    /// `canonical_slug`) is rejected. A real `id` is **accepted** even when `:free` or
    /// HF-mismatched, but those two cases return a **non-blocking warning** (surfaced,
    /// not rejected). Fetch failure → `Err` (fail closed; do not store). The endpoint's
    /// own `hugging_face_id` is required (proposal rule 2) so the mismatch warning has a
    /// reference and the listed row carries provenance.
    pub async fn validate_openrouter_slug(
        &self,
        slug: &str,
        endpoint_hf_id: Option<&str>,
    ) -> Result<SlugValidation> {
        let slug = slug.trim();
        if slug.is_empty() {
            return Err(Error::CommonError(
                "openrouter_slug is empty; a listing requires a resolved catalog id".into(),
            ));
        }
        let ep_hf = endpoint_hf_id.unwrap_or("").trim();
        if ep_hf.is_empty() {
            return Err(Error::CommonError(
                "hugging_face_id is required before setting an openrouter_slug (provenance + join key)"
                    .into(),
            ));
        }

        let data = self.fetch_openrouter_catalog().await?;
        ValidateSlugInCatalog(slug, ep_hf, &data)
    }

    pub(crate) fn EnsurePlatformEndpointAdmin(&self, token: &Arc<AccessToken>) -> Result<()> {
        if !token.IsNamespaceAdmin(PLATFORM_TENANT, PLATFORM_SHARED_NAMESPACE) {
            return Err(Error::NoPermission);
        }

        Ok(())
    }

    async fn GetPlatformEndpointFunc(&self, slug: &str) -> Result<Function> {
        let obj = self
            .client
            .Get(
                Function::KEY,
                PLATFORM_TENANT,
                PLATFORM_SHARED_NAMESPACE,
                slug,
                0,
            )
            .await?
            .ok_or_else(|| {
                Error::NotExist(format!(
                    "endpoint function {}/{}/{} does not exist",
                    PLATFORM_TENANT, PLATFORM_SHARED_NAMESPACE, slug
                ))
            })?;

        Ok(Function::FromDataObject(obj)?)
    }

    async fn GetPlatformEndpointStatus(&self, slug: &str) -> Result<FunctionStatus> {
        let obj = self
            .client
            .Get(
                FunctionStatus::KEY,
                PLATFORM_TENANT,
                PLATFORM_SHARED_NAMESPACE,
                slug,
                0,
            )
            .await?
            .ok_or_else(|| {
                Error::NotExist(format!(
                    "endpoint funcstatus {}/{}/{} does not exist",
                    PLATFORM_TENANT, PLATFORM_SHARED_NAMESPACE, slug
                ))
            })?;

        Ok(obj.To::<FunctionStatusDef>()?)
    }

    pub async fn Rbac(
        &self,
        token: &Arc<AccessToken>,
        permissionType: &PermissionType,
        objType: &ObjectType,
        tenant: &str,
        namespace: &str,
        name: &str,
        role: UserRole,
        username: &str,
    ) -> Result<()> {
        match objType {
            ObjectType::Tenant => {
                if tenant != "system" || namespace != "system" {
                    return Err(Error::CommonError(format!(
                        "invalid tenant or namespace name"
                    )));
                }

                match permissionType {
                    PermissionType::Grant => {
                        return self.GrantTenantRole(token, name, role, username).await;
                    }
                    PermissionType::Revoke => {
                        return self.RevokeTenantRole(token, name, role, username).await;
                    }
                }
            }
            ObjectType::Namespace => {
                if namespace != "system" {
                    return Err(Error::CommonError(format!(
                        "invalid tenant or namespace name"
                    )));
                }

                match permissionType {
                    PermissionType::Grant => {
                        return self
                            .GrantNamespaceRole(token, tenant, name, role, username)
                            .await;
                    }
                    PermissionType::Revoke => {
                        return self
                            .RevokeNamespaceRole(token, tenant, name, role, username)
                            .await;
                    }
                }
            }
        }
    }

    pub async fn GrantTenantRole(
        &self,
        token: &Arc<AccessToken>,
        tenantname: &str,
        role: UserRole,
        username: &str,
    ) -> Result<()> {
        match self.objRepo.tenantMgr.Get("system", "system", tenantname) {
            Err(e) => {
                if tenantname != AccessToken::SYSTEM_TENANT {
                    return Err(Error::NotExist(format!(
                        "fail to get tenant {} with error {:?}",
                        tenantname, e
                    )));
                }
            }
            Ok(_o) => (),
        };

        if !token.IsInferxAdmin() && !token.IsTenantAdmin(tenantname) {
            return Err(Error::NoPermission);
        }

        let usertoken = match GetTokenCache().await.GetTokenByUsername(username).await {
            Err(e) => {
                return Err(Error::NotExist(format!(
                    "fail to get access token for user {} with error {:?}",
                    tenantname, e
                )));
            }
            Ok(t) => t,
        };

        match role {
            UserRole::Admin => {
                if usertoken.IsTenantAdmin(tenantname) {
                    return Err(Error::Exist(format!(
                        "user {} already tenant {} admin",
                        username, tenantname
                    )));
                }
                GetTokenCache()
                    .await
                    .GrantTenantAdminPermission(&usertoken, tenantname, username)
                    .await?;
            }
            UserRole::User => {
                if usertoken.IsTenantUser(tenantname) {
                    return Err(Error::Exist(format!(
                        "user {} already tenant {} user",
                        username, tenantname
                    )));
                }
                GetTokenCache()
                    .await
                    .GrantTenantUserPermission(&usertoken, tenantname, username)
                    .await?;
            }
        }

        return Ok(());
    }

    pub async fn RevokeTenantRole(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        role: UserRole,
        username: &str,
    ) -> Result<()> {
        match self.objRepo.tenantMgr.Get("system", "system", tenant) {
            Err(e) => {
                if tenant != AccessToken::SYSTEM_TENANT {
                    return Err(Error::NotExist(format!(
                        "fail to get tenant {} with error {:?}",
                        tenant, e
                    )));
                }
            }
            Ok(_o) => (),
        };

        if !token.IsInferxAdmin() && !token.IsTenantAdmin(tenant) {
            return Err(Error::NoPermission);
        }

        let usertoken = match GetTokenCache().await.GetTokenByUsername(username).await {
            Err(e) => {
                return Err(Error::NotExist(format!(
                    "fail to get access token for user {} with error {:?}",
                    tenant, e
                )));
            }
            Ok(t) => t,
        };

        match role {
            UserRole::Admin => {
                if !usertoken.IsTenantAdmin(tenant) {
                    return Err(Error::NotExist(format!(
                        "user {} is not tenant {} admin",
                        username, tenant
                    )));
                }
                GetTokenCache()
                    .await
                    .RevokeTenantAdminPermission(&usertoken, tenant, username)
                    .await?;
            }
            UserRole::User => {
                if !usertoken.IsTenantUser(tenant) {
                    return Err(Error::NotExist(format!(
                        "user {} already tenant {} user",
                        username, tenant
                    )));
                }
                GetTokenCache()
                    .await
                    .RevokeTenantUserPermission(&usertoken, tenant, username)
                    .await?;
            }
        }

        return Ok(());
    }

    pub async fn GrantNamespaceRole(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        role: UserRole,
        username: &str,
    ) -> Result<()> {
        match self.objRepo.namespaceMgr.Get(tenant, "system", namespace) {
            Err(e) => {
                return Err(Error::NotExist(format!(
                    "fail to get namespace {}/{} with error {:?}",
                    tenant, namespace, e
                )));
            }
            Ok(_o) => (),
        };

        if !token.IsInferxAdmin()
            && !token.IsTenantAdmin(tenant)
            && !token.IsNamespaceAdmin(tenant, namespace)
        {
            return Err(Error::NoPermission);
        }

        let usertoken = match GetTokenCache().await.GetTokenByUsername(username).await {
            Err(e) => {
                return Err(Error::NotExist(format!(
                    "fail to get access token for user {} with error {:?}",
                    tenant, e
                )));
            }
            Ok(t) => t,
        };

        match role {
            UserRole::Admin => {
                if usertoken.IsNamespaceAdmin(tenant, namespace) {
                    return Err(Error::Exist(format!(
                        "user {} already namespace {}/{} admin",
                        username, tenant, namespace
                    )));
                }
                GetTokenCache()
                    .await
                    .GrantNamespaceAdminPermission(&usertoken, tenant, namespace, username)
                    .await?;
            }
            UserRole::User => {
                if usertoken.IsNamespaceUser(tenant, namespace) {
                    return Err(Error::Exist(format!(
                        "user {} already namespace {}/{} admin",
                        username, tenant, namespace
                    )));
                }
                GetTokenCache()
                    .await
                    .GrantNamespaceUserPermission(&usertoken, tenant, namespace, username)
                    .await?;
            }
        }

        return Ok(());
    }

    pub async fn RevokeNamespaceRole(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        role: UserRole,
        username: &str,
    ) -> Result<()> {
        match self.objRepo.namespaceMgr.Get(tenant, "system", namespace) {
            Err(e) => {
                return Err(Error::NotExist(format!(
                    "fail to get namespace {}/{} with error {:?}",
                    tenant, namespace, e
                )));
            }
            Ok(_o) => (),
        };

        if !token.IsInferxAdmin()
            && !token.IsTenantAdmin(tenant)
            && !token.IsNamespaceAdmin(tenant, namespace)
        {
            return Err(Error::NoPermission);
        }

        let usertoken = match GetTokenCache().await.GetTokenByUsername(username).await {
            Err(e) => {
                return Err(Error::NotExist(format!(
                    "fail to get access token for user {} with error {:?}",
                    tenant, e
                )));
            }
            Ok(t) => t,
        };

        match role {
            UserRole::Admin => {
                if !usertoken.IsNamespaceAdmin(tenant, namespace) {
                    return Err(Error::NotExist(format!(
                        "user {} is not namespace {}/{} admin",
                        username, tenant, namespace
                    )));
                }
                GetTokenCache()
                    .await
                    .RevokeNamespaceAdminPermission(&usertoken, tenant, namespace, username)
                    .await?;
            }
            UserRole::User => {
                if !usertoken.IsNamespaceUser(tenant, namespace) {
                    return Err(Error::Exist(format!(
                        "user {} is not namespace {}/{} admin",
                        username, tenant, namespace
                    )));
                }
                GetTokenCache()
                    .await
                    .RevokeNamespaceUserPermission(&usertoken, tenant, namespace, username)
                    .await?;
            }
        }

        return Ok(());
    }

    pub async fn RbacTenantUsers(
        &self,
        token: &Arc<AccessToken>,
        role: &str,
        tenant: &str,
    ) -> Result<Vec<String>> {
        if !token.IsInferxAdmin() && !token.IsTenantAdmin(tenant) {
            return Err(Error::NoPermission);
        }

        let users = match role {
            "admin" => GetTokenCache().await.GetTenantAdmins(tenant).await?,
            "user" => GetTokenCache().await.GetTenantUsers(tenant).await?,
            _ => {
                return Err(Error::CommonError(format!(
                    "the role name {} is not valid",
                    role
                )))
            }
        };

        return Ok(users);
    }

    pub async fn RbacNamespaceUsers(
        &self,
        token: &Arc<AccessToken>,
        role: &str,
        tenant: &str,
        namespace: &str,
    ) -> Result<Vec<String>> {
        if !token.IsInferxAdmin()
            && !token.IsTenantAdmin(tenant)
            && !token.IsNamespaceAdmin(tenant, namespace)
        {
            return Err(Error::NoPermission);
        }

        let users = match role {
            "admin" => {
                GetTokenCache()
                    .await
                    .GetNamespaceAdmins(tenant, namespace)
                    .await?
            }
            "user" => {
                GetTokenCache()
                    .await
                    .GetNamespaceUsers(tenant, namespace)
                    .await?
            }
            _ => {
                return Err(Error::CommonError(format!(
                    "the role name {} is not valid",
                    role
                )))
            }
        };

        return Ok(users);
    }

    pub async fn Onboard(
        &self,
        token: &Arc<AccessToken>,
    ) -> Result<(String, bool, String, String)> {
        if token.username == "anonymous" || token.sourceIsApikey || token.subject.is_empty() {
            return Err(Error::NoPermission);
        }

        let sub = token.subject.clone();
        let username = token.username.clone();
        let display_name = token.display_name.clone();
        let email = token.email.clone();
        let token_cache = GetTokenCache().await;
        let sql = &token_cache.sqlstore;
        let mut onboarding_apikey = String::new();
        let mut onboarding_apikey_name = String::new();

        let mut created = false;
        let mut row = match sql.GetOnboardInfo(&sub).await? {
            Some(info) => info,
            None => {
                let (info, inserted_new) = self.CreatePendingOnboard(&sub, &username).await?;
                created = inserted_new;
                info
            }
        };

        if row.status == "complete" {
            return Ok((row.tenant_name, false, String::new(), String::new()));
        }

        if row.status == "failed" {
            sql.ResetOnboard(&sub).await?;
            row.status = "pending".to_owned();
        }

        if row.username != username {
            sql.UpdateOnboardUsername(&sub, &username).await?;
            row.username = username.clone();
        }

        if row.status != "pending" {
            return Err(Error::CommonError(format!(
                "invalid UserOnboard status {} for sub {}",
                row.status, sub
            )));
        }

        let mut restarts = 0;
        loop {
            if row.saga_step < 1 {
                match self.CreateOnboardTenant(&row.tenant_name).await {
                    Ok(()) => {
                        sql.UpdateOnboardStep(&sub, 1).await?;
                        row.saga_step = 1;
                    }
                    Err(e) => {
                        if IsCreateConflictError(&e) {
                            if restarts >= ONBOARD_MAX_ATTEMPTS {
                                sql.MarkOnboardFailed(&sub).await?;
                                return Err(Error::CommonError(format!(
                                    "onboard exhausted tenant_name retries for sub {}",
                                    sub
                                )));
                            }

                            restarts += 1;
                            sql.DeleteOnboard(&sub).await?;
                            let (new_row, _) = self.CreatePendingOnboard(&sub, &username).await?;
                            row = new_row;
                            created = true;
                            continue;
                        }

                        return Err(e);
                    }
                }
            }

            if row.saga_step < 2 {
                self.EnsureRole(
                    token,
                    &username,
                    &AccessToken::TenantAdminRole(&row.tenant_name),
                )
                .await?;
                sql.UpdateOnboardStep(&sub, 2).await?;
                row.saga_step = 2;
            }

            if row.saga_step < 3 {
                match self
                    .CreateOnboardNamespace(&row.tenant_name, ONBOARD_DEFAULT_NAMESPACE)
                    .await
                {
                    Ok(()) => (),
                    Err(e) => {
                        if !IsCreateConflictError(&e) {
                            return Err(e);
                        }
                    }
                }

                self.EnsureRole(
                    token,
                    &username,
                    &AccessToken::NamespaceAdminRole(&row.tenant_name, ONBOARD_DEFAULT_NAMESPACE),
                )
                .await?;
                sql.UpdateOnboardStep(&sub, 3).await?;
                row.saga_step = 3;
            }

            if row.saga_step < 4 {
                let (created_apikey, created_apikey_name) = self
                    .EnsureOnboardInferenceApikey(token, &row.tenant_name, &sub)
                    .await?;
                onboarding_apikey = created_apikey;
                onboarding_apikey_name = created_apikey_name;
                sql.UpdateOnboardStep(&sub, 4).await?;
                row.saga_step = 4;
            }

            if row.saga_step < 5 {
                self.EnsureOnboardInitialCredit(&row.tenant_name, &sub)
                    .await?;
                sql.UpdateOnboardStep(&sub, 5).await?;
                row.saga_step = 5;
            }

            sql.CompleteOnboardWithProfile(&sub, &row.tenant_name, &display_name, &email)
                .await?;
            return Ok((
                row.tenant_name.clone(),
                created,
                onboarding_apikey,
                onboarding_apikey_name,
            ));
        }
    }

    async fn CreatePendingOnboard(
        &self,
        sub: &str,
        username: &str,
    ) -> Result<(super::secret::OnboardInfo, bool)> {
        let token_cache = GetTokenCache().await;
        let sql = &token_cache.sqlstore;

        for _ in 0..ONBOARD_MAX_ATTEMPTS {
            let tenant_name = GenerateTenantName();

            if self.TenantExistsInStore(&tenant_name).await? {
                continue;
            }

            match sql.InsertOnboard(sub, username, &tenant_name).await {
                Ok(true) => {
                    let row = sql.GetOnboardInfo(sub).await?;
                    match row {
                        Some(info) => return Ok((info, true)),
                        None => {
                            return Err(Error::CommonError(format!(
                                "inserted UserOnboard but cannot read sub {}",
                                sub
                            )));
                        }
                    }
                }
                Ok(false) => {
                    let row = sql.GetOnboardInfo(sub).await?;
                    match row {
                        Some(info) => return Ok((info, false)),
                        None => {
                            return Err(Error::CommonError(format!(
                                "UserOnboard conflict but cannot read sub {}",
                                sub
                            )));
                        }
                    }
                }
                Err(e) => {
                    if IsOnboardTenantConflictError(&e) {
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        return Err(Error::CommonError(format!(
            "failed to allocate unique tenant_name after {} attempts",
            ONBOARD_MAX_ATTEMPTS
        )));
    }

    async fn CreateOnboardTenant(&self, tenant_name: &str) -> Result<()> {
        if self
            .client
            .Get(Tenant::KEY, SYSTEM_TENANT, SYSTEM_NAMESPACE, tenant_name, 0)
            .await?
            .is_some()
        {
            return Ok(());
        }

        let tenant = Tenant {
            objType: Tenant::KEY.to_owned(),
            tenant: SYSTEM_TENANT.to_owned(),
            namespace: SYSTEM_NAMESPACE.to_owned(),
            name: tenant_name.to_owned(),
            labels: Default::default(),
            annotations: Default::default(),
            channelRev: 0,
            srcEpoch: 0,
            revision: 0,
            object: TenantObject::default(),
        };

        self.client.Create(&tenant.DataObject()).await?;
        return Ok(());
    }

    async fn CreateOnboardNamespace(&self, tenant_name: &str, namespace: &str) -> Result<()> {
        if self
            .client
            .Get(Namespace::KEY, tenant_name, SYSTEM_NAMESPACE, namespace, 0)
            .await?
            .is_some()
        {
            return Ok(());
        }

        let ns = Namespace {
            objType: Namespace::KEY.to_owned(),
            tenant: tenant_name.to_owned(),
            namespace: SYSTEM_NAMESPACE.to_owned(),
            name: namespace.to_owned(),
            labels: Default::default(),
            annotations: Default::default(),
            channelRev: 0,
            srcEpoch: 0,
            revision: 0,
            object: NamespaceObject::default(),
        };

        self.client.Create(&ns.DataObject()).await?;
        return Ok(());
    }

    pub async fn EnsurePlatformShared(&self) -> Result<()> {
        match self.CreateOnboardTenant(PLATFORM_TENANT).await {
            Ok(()) => {}
            Err(Error::NewKeyExistsErr(_)) | Err(Error::Exist(_)) => {}
            Err(e) => return Err(e),
        }
        match self.CreateOnboardNamespace(PLATFORM_TENANT, PLATFORM_SHARED_NAMESPACE).await {
            Ok(()) => {}
            Err(Error::NewKeyExistsErr(_)) | Err(Error::Exist(_)) => {}
            Err(e) => return Err(e),
        }
        self.ReconcileInferxTenantPolicy().await?;
        return Ok(());
    }

    async fn ReconcileInferxTenantPolicy(&self) -> Result<()> {
        let policy = &GATEWAY_CONFIG.inferxTenantPolicy;
        if !Self::HasInferxTenantPolicyOverrides(policy) {
            return Ok(());
        }

        let mut attempts = 0;
        loop {
            attempts += 1;
            let tenant_obj = self
                .client
                .Get(Tenant::KEY, SYSTEM_TENANT, SYSTEM_NAMESPACE, PLATFORM_TENANT, 0)
                .await?;
            let tenant_obj = match tenant_obj {
                Some(obj) => obj,
                None => {
                    return Err(Error::NotExist(format!(
                        "platform tenant {} does not exist during reconcile",
                        PLATFORM_TENANT
                    )));
                }
            };
            let mut tenant_obj = Tenant::FromDataObject(tenant_obj)?;
            let changed = Self::ApplyInferxTenantPolicy(&mut tenant_obj, policy);

            if !changed {
                return Ok(());
            }

            match self
                .client
                .Update(&tenant_obj.DataObject(), tenant_obj.revision)
                .await
            {
                Ok(_) => return Ok(()),
                Err(e) if attempts < 3 && IsCasConflictError(&e) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    async fn EnsureRole(&self, token: &Arc<AccessToken>, username: &str, role: &str) -> Result<()> {
        let token_cache = GetTokenCache().await;
        match token_cache.sqlstore.AddRole(username, role).await {
            Ok(()) => {
                token_cache.EvactionToken(token);
                return Ok(());
            }
            Err(e) => {
                if IsUniqueViolationError(&e) {
                    token_cache.EvactionToken(token);
                    return Ok(());
                }
                return Err(e);
            }
        }
    }

    async fn EnsureOnboardInferenceApikey(
        &self,
        token: &Arc<AccessToken>,
        tenant_name: &str,
        sub: &str,
    ) -> Result<(String, String)> {
        let keyname = BuildOnboardInferenceApikeyName(tenant_name, sub);
        let req = ApikeyCreateRequest {
            username: "".to_owned(),
            keyname: keyname.clone(),
            access_level: Some("inference".to_owned()),
            restrict_tenant: Some(tenant_name.to_owned()),
            restrict_namespace: None,
            expires_in_days: None,
        };
        let token_cache = GetTokenCache().await;

        match token_cache.CreateApikey(token, &req).await {
            Ok(resp) => Ok((resp.apikey, keyname)),
            Err(e) => {
                if IsOnboardApikeyConflictError(&e) {
                    let keys = token_cache.GetApikeys(&token.username).await?;
                    for k in keys {
                        if k.keyname == keyname {
                            return Ok((k.apikey, k.keyname));
                        }
                    }
                    return Ok((String::new(), keyname));
                }

                return Err(e);
            }
        }
    }

    async fn EnsureOnboardInitialCredit(&self, tenant_name: &str, sub: &str) -> Result<()> {
        let amount_cents = GATEWAY_CONFIG.onboardInitialCreditCents;
        if amount_cents <= 0 {
            return Ok(());
        }

        let payment_ref = BuildOnboardInitialCreditPaymentRef(tenant_name, sub);
        if self
            .sqlBilling
            .HasTenantCreditPaymentRef(tenant_name, &payment_ref)
            .await?
        {
            return Ok(());
        }

        self.sqlBilling
            .AddTenantCredit(
                tenant_name,
                amount_cents,
                "USD",
                Some(ONBOARD_INITIAL_CREDIT_NOTE),
                Some(&payment_ref),
                Some(ONBOARD_INITIAL_CREDIT_ADDED_BY),
            )
            .await?;

        let quota_exceeded = self.sqlBilling.RecalculateTenantQuota(tenant_name).await?;
        let tenant_obj = self
            .client
            .Get(Tenant::KEY, SYSTEM_TENANT, SYSTEM_NAMESPACE, tenant_name, 0)
            .await?;
        let tenant_obj = match tenant_obj {
            Some(obj) => obj,
            None => {
                return Err(Error::NotExist(format!(
                    "onboard tenant {} does not exist while adding initial credit",
                    tenant_name
                )));
            }
        };
        let mut tenant_obj = Tenant::FromDataObject(tenant_obj)?;
        if tenant_obj.object.status.quota_exceeded != quota_exceeded {
            tenant_obj.object.status.quota_exceeded = quota_exceeded;
            self.client.Update(&tenant_obj.DataObject(), 0).await?;
        }

        Ok(())
    }

    async fn TenantExistsInStore(&self, tenant_name: &str) -> Result<bool> {
        let obj = self
            .client
            .Get(Tenant::KEY, SYSTEM_TENANT, SYSTEM_NAMESPACE, tenant_name, 0)
            .await?;
        return Ok(obj.is_some());
    }

    pub async fn CreateTenant(
        &self,
        token: &Arc<AccessToken>,
        obj: DataObject<Value>,
    ) -> Result<i64> {
        if !token.IsInferxAdmin() {
            return Err(Error::NoPermission);
        }

        let tenant: Tenant = Tenant::FromDataObject(obj.clone())?;

        let tenantName = tenant.tenant.clone();

        if &tenantName != "system" || &tenant.namespace != "system" {
            return Err(Error::CommonError(format!(
                "invalid tenant or namespace name"
            )));
        }

        if !token.IsInferxAdmin() {
            return Err(Error::NoPermission);
        }

        let version = self.client.Create(&obj).await?;

        // GetTokenCache()
        //     .await
        //     .GrantTenantAdminPermission(token, &tenant.name, &token.username)
        //     .await?;

        return Ok(version);
    }

    pub async fn UpdateTenant(
        &self,
        token: &Arc<AccessToken>,
        obj: DataObject<Value>,
    ) -> Result<i64> {
        let tenant: Tenant = Tenant::FromDataObject(obj.clone())?;

        // Only allow updating tenant in system/system
        if &tenant.tenant != "system" || &tenant.namespace != "system" {
            return Err(Error::CommonError(format!(
                "invalid tenant or namespace name"
            )));
        }

        // Check permission - must be tenant admin
        if !token.IsTenantAdmin(&tenant.name) {
            return Err(Error::NoPermission);
        }

        let version = self.client.Update(&obj, 0).await?;
        return Ok(version);
    }

    pub async fn DeleteTenant(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        name: &str,
    ) -> Result<i64> {
        if tenant != "system" || namespace != "system" {
            return Err(Error::CommonError(format!(
                "invalid tenant or namespace name"
            )));
        }

        if !token.IsInferxAdmin() {
            return Err(Error::NoPermission);
        }

        if !self.objRepo.namespaceMgr.IsEmpty(name, "system") {
            return Err(Error::CommonError(format!(
                "the tenant {} is not empty",
                tenant,
            )));
        }

        let version = self
            .client
            .Delete(Tenant::KEY, tenant, namespace, name, 0)
            .await?;

        return Ok(version);
    }

    pub async fn CreateNamespace(
        &self,
        token: &Arc<AccessToken>,
        obj: DataObject<Value>,
    ) -> Result<i64> {
        let ns: Namespace = Namespace::FromDataObject(obj.clone())?;

        let tenant = ns.tenant.clone();
        let namespace = ns.namespace.clone();

        if &namespace != "system" {
            return Err(Error::CommonError(format!("invalid namespace name")));
        }

        if !token.IsTenantAdmin(&tenant) {
            return Err(Error::NoPermission);
        }

        let version = self.client.Create(&obj).await?;
        GetTokenCache()
            .await
            .GrantNamespaceAdminPermission(token, &tenant, &ns.name, &token.username)
            .await?;

        return Ok(version);
    }

    pub async fn UpdateNamespace(
        &self,
        _token: &Arc<AccessToken>,
        _obj: DataObject<Value>,
    ) -> Result<i64> {
        return Err(Error::CommonError(format!(
            "doesn't support namespace update"
        )));
    }

    pub async fn DeleteNamespace(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        name: &str,
    ) -> Result<i64> {
        if namespace != "system" {
            return Err(Error::CommonError(format!(
                "invalid tenant or namespace name"
            )));
        }

        if !self.objRepo.funcMgr.IsEmpty(tenant, name) {
            return Err(Error::CommonError(format!(
                "the tenant {} is not empty",
                tenant,
            )));
        }

        if !token.IsTenantAdmin(tenant) {
            return Err(Error::NoPermission);
        }

        let version = self
            .client
            .Delete(Namespace::KEY, tenant, namespace, name, 0)
            .await?;

        GetTokenCache()
            .await
            .RevokeNamespaceAdminPermission(token, tenant, name, &token.username)
            .await?;

        return Ok(version);
    }

    pub async fn CreateFunc(
        &self,
        token: &Arc<AccessToken>,
        obj: DataObject<Value>,
    ) -> Result<i64> {
        let mut dataobj = obj;

        let mut func: Function = Function::FromDataObject(dataobj)?;

        let tenant = func.tenant.clone();
        let namespace = func.namespace.clone();

        if !token.IsNamespaceAdmin(&tenant, &namespace) {
            return Err(Error::NoPermission);
        }

        let id = self.client.Uid().await?;
        func.object.spec.version = id;
        dataobj = func.DataObject();

        let kb_paths =
            if func.object.spec.SampleCallType() == inferxlib::obj_mgr::func_mgr::ApiType::KnowledgeBase
            {
                Some(Self::prepare_kb_stage_promotion(
                    &tenant, &namespace, &func.name, id,
                )?)
            } else {
                None
            };

        match self.client.Create(&dataobj).await {
            Ok(version) => Ok(version),
            Err(e) => {
                if let Some((stage_file, version_dir, version_file)) = kb_paths {
                    Self::rollback_kb_stage_promotion(&stage_file, &version_dir, &version_file);
                }
                Err(e)
            }
        }
    }

    pub async fn CreateFuncPolicy(
        &self,
        token: &Arc<AccessToken>,
        obj: DataObject<Value>,
    ) -> Result<i64> {
        let dataobj = obj;
        let tenant = dataobj.tenant.clone();
        let namespace = dataobj.namespace.clone();

        if !token.IsNamespaceAdmin(&tenant, &namespace) {
            return Err(Error::NoPermission);
        }

        return self.client.Create(&dataobj).await;
    }

    pub async fn UpdateFunc(
        &self,
        token: &Arc<AccessToken>,
        obj: DataObject<Value>,
    ) -> Result<i64> {
        if !token.IsNamespaceAdmin(&obj.tenant, &obj.namespace) {
            return Err(Error::NoPermission);
        }

        let mut dataobj = obj;
        let mut func = Function::FromDataObject(dataobj)?;
        let tenant = func.tenant.clone();
        let namespace = func.namespace.clone();
        let name = func.name.clone();
        let existing = self
            .client
            .Get(Function::KEY, &tenant, &namespace, &name, 0)
            .await?
            .ok_or_else(|| {
                Error::NotExist(format!("function {}/{}/{} does not exist", tenant, namespace, name))
            })?
            .To::<inferxlib::obj_mgr::func_mgr::FuncObject>()?;
        let old_version = existing.object.spec.version;
        let id = self.client.Uid().await?;
        func.object.spec.version = id;
        func.object.status = FuncStatus::default();
        dataobj = func.DataObject();
        let kb_promotion =
            if func.object.spec.SampleCallType() == inferxlib::obj_mgr::func_mgr::ApiType::KnowledgeBase
            {
                let stage_file = kb_stage_file_path(&tenant, &namespace, &name);
                if stage_file.exists() {
                    let (stage_file, version_dir, version_file) =
                        Self::prepare_kb_stage_promotion(&tenant, &namespace, &name, id)?;
                    Some((false, stage_file, version_dir, version_file))
                } else {
                    let version_dir =
                        Self::prepare_kb_version_copy(&tenant, &namespace, &name, old_version, id)?;
                    Some((true, PathBuf::new(), version_dir, PathBuf::new()))
                }
            } else {
                None
            };

        match self.client.Update(&dataobj, 0).await {
            Ok(version) => Ok(version),
            Err(e) => {
                if let Some((copied, stage_file, version_dir, version_file)) = kb_promotion {
                    if copied {
                        Self::cleanup_kb_version_dir(&version_dir);
                    } else {
                        Self::rollback_kb_stage_promotion(&stage_file, &version_dir, &version_file);
                    }
                }
                Err(e)
            }
        }
    }

    pub async fn UpdateFuncPolicy(
        &self,
        token: &Arc<AccessToken>,
        obj: DataObject<Value>,
    ) -> Result<i64> {
        let dataobj = obj;
        let tenant = dataobj.tenant.clone();
        let namespace = dataobj.namespace.clone();

        if !token.IsNamespaceAdmin(&tenant, &namespace) {
            return Err(Error::NoPermission);
        }

        return self.client.Update(&dataobj, 0).await;
    }

    pub async fn DeleteFunc(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        name: &str,
    ) -> Result<i64> {
        if !token.IsNamespaceAdmin(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let version = self
            .client
            .Delete(Function::KEY, tenant, namespace, name, 0)
            .await?;

        return Ok(version);
    }

    pub async fn DeleteFuncPolicy(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        name: &str,
    ) -> Result<i64> {
        if !token.IsNamespaceAdmin(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let version = self
            .client
            .Delete(FuncPolicy::KEY, tenant, namespace, name, 0)
            .await?;

        return Ok(version);
    }

    pub async fn ListTenant(
        &self,
        token: &Arc<AccessToken>,
        _tenant: &str,
        _namespace: &str,
    ) -> Result<Vec<DataObject<Value>>> {
        let tenants = self.UserTenants(token);
        let mut objs = Vec::new();
        for tenant in &tenants {
            let obj = match self.objRepo.tenantMgr.Get("system", "system", tenant) {
                Err(e) => {
                    error!("ListTenant fail with error {:?}", e);
                    continue;
                }
                Ok(o) => o.DataObject(),
            };
            objs.push(obj);
        }

        return Ok(objs);
    }

    pub async fn ListNamespace(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
    ) -> Result<Vec<DataObject<Value>>> {
        let mut objs = Vec::new();
        if tenant == "" {
            let namespaces = self.UserNamespaces(token);

            for (tenant, namespace) in &namespaces {
                let obj = match self.objRepo.namespaceMgr.Get(tenant, "system", namespace) {
                    Err(e) => {
                        error!("ListNamespace fail with error {:?}", e);
                        continue;
                    }
                    Ok(o) => o.DataObject(),
                };
                objs.push(obj);
            }
        } else if namespace == "" {
            let namespaces = self.UserNamespaces(token);
            for (t, namespace) in &namespaces {
                if t != tenant {
                    continue;
                }
                let obj = match self.objRepo.namespaceMgr.Get(tenant, "system", namespace) {
                    Err(e) => {
                        error!("ListNamespace fail with error {:?}", e);
                        continue;
                    }
                    Ok(o) => o.DataObject(),
                };
                objs.push(obj);
            }
        } else {
            match self.objRepo.namespaceMgr.Get(tenant, "system", namespace) {
                Err(e) => {
                    error!("ListNamespace fail with error {:?}", e);
                }
                Ok(o) => objs.push(o.DataObject()),
            };
        }

        return Ok(objs);
    }

    pub async fn ListFunc(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
    ) -> Result<Vec<DataObject<Value>>> {
        // TODO: Enforce the same read-scope + namespace-user/admin checks as ListFuncBrief
        // for /objects/function/* to avoid weaker authorization on this generic list path.
        let mut objs = Vec::new();
        if tenant == "" {
            let namespaces = self.AdminNamespaces(token);

            for (tenant, namespace) in &namespaces {
                match self.objRepo.funcMgr.GetObjects(tenant, namespace) {
                    Err(e) => {
                        error!("ListFunc fail with error {:?}", e);
                    }
                    Ok(o) => {
                        for f in o {
                            objs.push(f.DataObject());
                        }
                    }
                };
            }
        } else if namespace == "" {
            let namespaces = self.AdminNamespaces(token);
            for (t, namespace) in &namespaces {
                if t != tenant {
                    continue;
                }
                match self.objRepo.funcMgr.GetObjects(tenant, namespace) {
                    Err(e) => {
                        error!("ListFunc fail with error {:?}", e);
                    }
                    Ok(o) => {
                        for f in o {
                            objs.push(f.DataObject());
                        }
                    }
                };
            }
        } else {
            match self.objRepo.funcMgr.GetObjects(tenant, namespace) {
                Err(e) => {
                    error!("ListFunc fail with error {:?}", e);
                }
                Ok(o) => {
                    for f in o {
                        objs.push(f.DataObject());
                    }
                }
            };
        }

        return Ok(objs);
    }

    pub async fn ListObj(
        &self,
        token: &Arc<AccessToken>,
        objType: &str,
        tenant: &str,
        namespace: &str,
    ) -> Result<Vec<DataObject<Value>>> {
        match objType {
            Tenant::KEY => return self.ListTenant(token, tenant, namespace).await,
            Namespace::KEY => return self.ListNamespace(token, tenant, namespace).await,
            Function::KEY => return self.ListFunc(token, tenant, namespace).await,
            _ => (),
        }

        let mut objs = Vec::new();
        if tenant == "" {
            let namespaces = self.UserNamespaces(token);

            for (tenant, namespace) in namespaces {
                let mut list = self
                    .client
                    .List(&objType, &tenant, &namespace, &ListOption::default())
                    .await?;
                objs.append(&mut list.objs);
            }

            let mut list = self
                .client
                .List(&objType, "public", "", &ListOption::default())
                .await?;
            objs.append(&mut list.objs);
        } else if namespace == "" {
            let namespaces = self.UserNamespaces(token);
            for (currentTenant, namespace) in namespaces {
                if &currentTenant != tenant {
                    continue;
                }
                let mut list = self
                    .client
                    .List(&objType, &tenant, &namespace, &ListOption::default())
                    .await?;
                objs.append(&mut list.objs);
            }
        } else {
            let mut list = self
                .client
                .List(&objType, &tenant, &namespace, &ListOption::default())
                .await?;
            objs.append(&mut list.objs);
        }

        return Ok(objs);
    }

    pub fn UserTenants(&self, token: &Arc<AccessToken>) -> Vec<String> {
        if token.IsInferxAdmin() {
            let mut tenants = Vec::new();
            for ns in self.objRepo.tenantMgr.GetObjects("", "").unwrap() {
                tenants.push(ns.Name());
            }

            return tenants;
        }

        return token.UserTenants();
    }

    pub fn AdminTenants(&self, token: &Arc<AccessToken>) -> Vec<String> {
        if token.IsInferxAdmin() {
            let mut tenants = Vec::new();
            for ns in self.objRepo.tenantMgr.GetObjects("", "").unwrap() {
                tenants.push(ns.Name());
            }

            return tenants;
        }

        return token.AdminTenants();
    }

    pub fn AdminNamespaces(&self, token: &Arc<AccessToken>) -> Vec<(String, String)> {
        if token.IsInferxAdmin() {
            let mut namespaces = Vec::new();
            for ns in self.objRepo.namespaceMgr.GetObjects("", "").unwrap() {
                namespaces.push((ns.Tenant(), ns.Name()))
            }

            return namespaces;
        }

        let mut namespaces = BTreeSet::new();
        for (tenant, namespace) in token.AdminNamespaces() {
            namespaces.insert((tenant, namespace));
        }

        // Tenant admins implicitly administer all namespaces in their tenant.
        for tenant in token.AdminTenants() {
            match self.objRepo.namespaceMgr.GetObjects(&tenant, "") {
                Ok(items) => {
                    for ns in items {
                        namespaces.insert((ns.Tenant(), ns.Name()));
                    }
                }
                Err(e) => {
                    error!(
                        "AdminNamespaces fail to list tenant {} namespaces: {:?}",
                        tenant, e
                    );
                }
            }
        }

        return namespaces.into_iter().collect();
    }

    pub fn UserNamespaces(&self, token: &Arc<AccessToken>) -> Vec<(String, String)> {
        if token.IsInferxAdmin() {
            let mut namespaces = Vec::new();
            for ns in self.objRepo.namespaceMgr.GetObjects("", "").unwrap() {
                namespaces.push((ns.Tenant(), ns.Name()))
            }

            return namespaces;
        }

        // Start with explicit namespace roles.
        let mut namespaces: BTreeSet<(String, String)> =
            token.UserNamespaces().into_iter().collect();

        // Expand tenant-user/admin permissions to all namespaces under those tenants.
        for tenant in token.UserTenants() {
            match self.objRepo.namespaceMgr.GetObjects(&tenant, "") {
                Ok(list) => {
                    for ns in list {
                        namespaces.insert((ns.Tenant(), ns.Name()));
                    }
                }
                Err(e) => {
                    error!(
                        "UserNamespaces: failed to list namespaces for tenant {}: {:?}",
                        tenant, e
                    );
                }
            }
        }

        return namespaces.into_iter().collect();
    }

    pub fn ListFuncBrief(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
    ) -> Result<Vec<FuncBrief>> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        let mut objs = Vec::new();
        if tenant == "" {
            let namespaces = self.UserNamespaces(token);

            for (tenant, namespace) in namespaces {
                if &tenant != "public" {
                    let mut list = self.objRepo.ListFunc(&tenant, &namespace)?;
                    objs.append(&mut list);
                }
            }

            let mut list = self.objRepo.ListFunc("public", &namespace)?;
            objs.append(&mut list);
        } else if namespace == "" {
            let namespaces = self.UserNamespaces(token);
            for (currentTenant, namespace) in namespaces {
                if &currentTenant != tenant {
                    continue;
                }
                let mut list = self.objRepo.ListFunc(&tenant, &namespace)?;
                objs.append(&mut list);
            }
        } else {
            if !token.IsNamespaceUser(tenant, namespace) {
                return Err(Error::NoPermission);
            }
            let mut list = self.objRepo.ListFunc(&tenant, &namespace)?;
            objs.append(&mut list);
        }

        return Ok(objs);
    }

    pub fn GetFuncDetail(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        funcname: &str,
    ) -> Result<FuncDetail> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        if !token.IsNamespaceUser(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let mut detail = self.objRepo.GetFuncDetail(&tenant, &namespace, &funcname)?;
        detail.isAdmin = token.IsNamespaceAdmin(tenant, namespace);
        return Ok(detail);
    }

    pub fn GetSnapshots(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
    ) -> Result<Vec<FuncSnapshot>> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        let mut objs = Vec::new();
        if tenant == "" {
            let namespaces = self.UserNamespaces(token);

            for (tenant, namespace) in namespaces {
                if &tenant == "public" {
                    continue;
                }
                let mut list = self.objRepo.GetSnapshots(&tenant, &namespace)?;
                objs.append(&mut list);
            }

            let mut list = self.objRepo.GetSnapshots("public", &namespace)?;
            objs.append(&mut list);
        } else if namespace == "" {
            let namespaces = self.UserNamespaces(token);
            for (currentTenant, namespace) in namespaces {
                if &currentTenant != tenant {
                    continue;
                }
                let mut list = self.objRepo.GetSnapshots(&tenant, &namespace)?;
                objs.append(&mut list);
            }
        } else {
            if !token.IsNamespaceUser(tenant, namespace) {
                return Err(Error::NoPermission);
            }
            let mut list = self.objRepo.GetSnapshots(&tenant, &namespace)?;
            objs.append(&mut list);
        }

        return Ok(objs);
    }

    pub fn GetSnapshot(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        funcname: &str,
    ) -> Result<FuncSnapshot> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        if !token.IsNamespaceUser(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let snapshot = self.objRepo.GetSnapshot(&tenant, &namespace, &funcname)?;
        return Ok(snapshot);
    }

    pub fn GetFuncPods(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        funcname: &str,
    ) -> Result<Vec<FuncPod>> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        let mut objs = Vec::new();
        if tenant == "" {
            let namespaces = self.UserNamespaces(token);

            for (tenant, namespace) in namespaces {
                if &tenant == "public" {
                    continue;
                }
                let mut list = self.objRepo.GetFuncPods(&tenant, &namespace, funcname)?;
                objs.append(&mut list);
            }

            let mut list = self.objRepo.GetFuncPods("public", &namespace, funcname)?;
            objs.append(&mut list);
        } else if namespace == "" {
            let namespaces = self.UserNamespaces(token);
            for (currentTenant, namespace) in namespaces {
                if &currentTenant != tenant {
                    continue;
                }
                let mut list = self.objRepo.GetFuncPods(&tenant, &namespace, funcname)?;
                objs.append(&mut list);
            }
        } else {
            if !token.IsNamespaceUser(tenant, namespace) {
                return Err(Error::NoPermission);
            }
            let mut list = self.objRepo.GetFuncPods(&tenant, &namespace, funcname)?;
            objs.append(&mut list);
        }

        return Ok(objs);
    }

    pub fn GetFuncPod(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        funcname: &str,
    ) -> Result<FuncPod> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        if !token.IsNamespaceUser(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let pod = self.objRepo.GetFuncPod(&tenant, &namespace, &funcname)?;
        return Ok(pod);
    }

    pub async fn ReadLog(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        version: i64,
        id: &str,
    ) -> Result<String> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        if !token.IsNamespaceUser(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let s = self
            .objRepo
            .GetFuncPodLog(&tenant, &namespace, &funcname, version, id)
            .await?;
        return Ok(s);
    }

    pub async fn ReadPodAuditLog(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        version: i64,
        id: &str,
    ) -> Result<Vec<PodAuditLog>> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        if !token.IsNamespaceUser(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let s = self
            .sqlAudit
            .ReadPodAudit(&tenant, &namespace, &funcname, version, id)
            .await?;
        return Ok(s);
    }

    pub async fn ReadSnapshotScheduleRecords(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        version: i64,
    ) -> Result<Vec<SnapshotScheduleAudit>> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        if !token.IsNamespaceUser(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let s = self
            .sqlAudit
            .ReadSnapshotScheduleRecords(&tenant, &namespace, &funcname, version)
            .await?;
        return Ok(s);
    }

    pub async fn ReadPodFailLogs(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        version: i64,
    ) -> Result<Vec<PodFailLog>> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        if !token.IsNamespaceUser(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let s = self
            .sqlAudit
            .ReadPodFailLogs(&tenant, &namespace, &funcname, version)
            .await?;
        return Ok(s);
    }

    pub async fn ReadPodFaillog(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        version: i64,
        id: &str,
    ) -> Result<PodFailLog> {
        if !token.CheckScope("read") {
            return Err(Error::NoPermission);
        }

        if !token.IsNamespaceUser(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let s = self
            .sqlAudit
            .ReadPodFailLog(&tenant, &namespace, &funcname, version, id)
            .await?;
        return Ok(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inferxlib::obj_mgr::tenant_mgr::{TenantObject, TenantSpec};

    fn make_tenant(name: &str) -> Tenant {
        Tenant {
            objType: Tenant::KEY.to_owned(),
            tenant: SYSTEM_TENANT.to_owned(),
            namespace: SYSTEM_NAMESPACE.to_owned(),
            name: name.to_owned(),
            object: TenantObject {
                spec: TenantSpec::default(),
                status: Default::default(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn inferx_tenant_policy_overrides_detects_max_gpu_only() {
        let policy = crate::node_config::InferxTenantPolicy {
            maxGpu: Some(4),
            ..Default::default()
        };

        assert!(HttpGateway::HasInferxTenantPolicyOverrides(&policy));
    }

    #[test]
    fn apply_inferx_tenant_policy_updates_max_gpu() {
        let mut tenant = make_tenant("inferx");
        let policy = crate::node_config::InferxTenantPolicy {
            maxGpu: Some(5),
            ..Default::default()
        };

        let changed = HttpGateway::ApplyInferxTenantPolicy(&mut tenant, &policy);

        assert!(changed);
        assert_eq!(tenant.object.spec.resourceLimit.maxGpu, 5);
    }
}

fn GenerateTenantName() -> String {
    const BASE36: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    let mut suffix = String::with_capacity(ONBOARD_TENANT_SUFFIX_LEN);
    for _ in 0..ONBOARD_TENANT_SUFFIX_LEN {
        let idx = rng.gen_range(0..BASE36.len());
        suffix.push(BASE36[idx] as char);
    }

    return format!("{ONBOARD_TENANT_PREFIX}{suffix}");
}

fn IsUniqueViolationError(err: &Error) -> bool {
    match err {
        Error::SqlxError(sqlx::Error::Database(db_err)) => {
            db_err.code().as_deref() == Some("23505")
        }
        _ => false,
    }
}

fn IsOnboardTenantConflictError(err: &Error) -> bool {
    match err {
        Error::SqlxError(sqlx::Error::Database(db_err)) => {
            if db_err.code().as_deref() != Some("23505") {
                return false;
            }

            if matches!(db_err.constraint(), Some("useronboard_idx_tenant_name")) {
                return true;
            }

            return db_err
                .message()
                .contains("duplicate key value violates unique constraint");
        }
        _ => false,
    }
}

fn IsOnboardApikeyConflictError(err: &Error) -> bool {
    match err {
        Error::SqlxError(sqlx::Error::Database(db_err)) => {
            let is_unique_violation = db_err.code().as_deref() == Some("23505");
            if !is_unique_violation {
                return false;
            }

            if matches!(
                db_err.constraint(),
                Some("apikey_idx_username_keyname") | Some("apikey_idx_realm_username")
            ) {
                return true;
            }

            let msg = db_err.message().to_ascii_lowercase();
            if !msg.contains("duplicate key value violates unique constraint") {
                return false;
            }

            let cst = db_err
                .constraint()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            cst.contains("apikey")
                || msg.contains("(username, keyname)")
                || msg.contains("apikey_idx_")
                || msg.contains("apikey")
        }
        _ => false,
    }
}

fn BuildOnboardInferenceApikeyName(tenant_name: &str, sub: &str) -> String {
    let mut tenant_slug_raw = String::new();
    let mut prev_dash = false;
    for c in tenant_name.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            tenant_slug_raw.push(c);
            prev_dash = false;
            continue;
        }

        if !prev_dash {
            tenant_slug_raw.push('-');
            prev_dash = true;
        }
    }

    let tenant_slug = tenant_slug_raw.trim_matches('-').to_owned();
    let tenant_slug = if tenant_slug.is_empty() {
        "tenant".to_owned()
    } else {
        tenant_slug
    };

    let sub_slug = sub
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>();
    let sub_suffix = if sub_slug.is_empty() {
        "user".to_owned()
    } else {
        sub_slug.chars().take(8).collect::<String>()
    };

    format!("{}-{}-{}", ONBOARD_APIKEY_PREFIX, tenant_slug, sub_suffix)
}

fn BuildOnboardInitialCreditPaymentRef(tenant_name: &str, sub: &str) -> String {
    let sub_slug = sub
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>();
    let sub_suffix = if sub_slug.is_empty() {
        "user".to_owned()
    } else {
        sub_slug.chars().take(16).collect::<String>()
    };

    format!(
        "{}:{}:{}",
        ONBOARD_INITIAL_CREDIT_PAYMENT_REF_PREFIX, tenant_name, sub_suffix
    )
}

fn IsCreateConflictError(err: &Error) -> bool {
    match err {
        Error::Exist(_) | Error::NewKeyExistsErr(_) => true,
        Error::CommonError(msg) => {
            let msg = msg.to_ascii_lowercase();
            msg.contains("already exist")
                || msg.contains("already exists")
                || msg.contains("key exists")
        }
        _ => false,
    }
}

fn IsCasConflictError(err: &Error) -> bool {
    match err {
        Error::UpdateRevNotMatchErr(_) => true,
        Error::CommonError(msg) => msg.contains("UpdateRevNotMatchErr"),
        _ => false,
    }
}

/// One pricing tier must carry per-token `prompt` and `completion` as strings.
fn IsValidPricingTier(tier: &Value) -> bool {
    let obj = match tier.as_object() {
        Some(o) => o,
        None => return false,
    };
    obj.get("prompt").and_then(|v| v.as_str()).is_some()
        && obj.get("completion").and_then(|v| v.as_str()).is_some()
}

/// `pricing` must be a single tier object or an array of up to 2 tiers,
/// each carrying string `prompt`/`completion`.
fn IsValidPricing(pricing: &Value) -> bool {
    match pricing {
        Value::Object(_) => IsValidPricingTier(pricing),
        Value::Array(tiers) => {
            !tiers.is_empty() && tiers.len() <= 2 && tiers.iter().all(IsValidPricingTier)
        }
        _ => false,
    }
}

/// Outcome of validating an admin-edited `openrouter_slug` against the live catalog
/// (§ Validation Rules). A real catalog `id` is accepted; `warnings` carries the
/// non-blocking signals (HF-id mismatch, `:free` pin) the admin should see and may
/// proceed past knowingly. A hard rejection (nonexistent slug, fetch failure, missing
/// HF id) is an `Err`, not a warning.
#[derive(Debug, Clone, Default)]
pub struct SlugValidation {
    pub warnings: Vec<String>,
}

/// Enforce the "don't ship incomplete/guessed metadata" rule: every
/// OpenRouter-required field must be present and well-formed before listing.
/// Returns a single error naming all missing/invalid fields.
pub fn ValidateOpenRouterListing(row: &ListedEndpoint) -> Result<()> {
    let mut problems: Vec<String> = Vec::new();

    let nonempty_vec = |v: &Option<Vec<String>>| matches!(v, Some(items) if !items.is_empty());

    // hugging_face_id is required on every listed endpoint (proposal rule 2): it is the
    // auto-discovery join key AND required provenance. A manual slug edit does not
    // exempt the row from carrying it.
    if row
        .hugging_face_id
        .as_ref()
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
    {
        problems.push("hugging_face_id (required — provenance + auto-discovery join key)".into());
    }
    if !nonempty_vec(&row.input_modalities) {
        problems.push("input_modalities (null/empty — set real values)".into());
    }
    if !nonempty_vec(&row.output_modalities) {
        problems.push("output_modalities (null/empty — set real values)".into());
    }
    if row
        .quantization
        .as_ref()
        .map(|q| q.trim().is_empty())
        .unwrap_or(true)
    {
        problems.push("quantization (documented enum value required)".into());
    }
    if row.context_length.is_none() {
        problems.push("context_length".into());
    }
    if row.max_output_length.is_none() {
        problems.push("max_output_length".into());
    }
    match &row.pricing {
        Some(p) if IsValidPricing(p) => {}
        Some(_) => problems.push("pricing (must be a tier object/array with string prompt+completion)".into()),
        None => problems.push("pricing".into()),
    }
    // supported_* are required fields; an empty list is allowed (genuinely none),
    // but the column must be set (not null) so it isn't silently synthesized.
    if row.supported_sampling_parameters.is_none() {
        problems.push("supported_sampling_parameters".into());
    }
    if row.supported_features.is_none() {
        problems.push("supported_features".into());
    }
    // NOTE: `last_published_at` is intentionally NOT required. OpenRouter serving is
    // decoupled from publish; `created` falls back to listing time (`or_listed_at`),
    // which `SetEndpointListed` stamps the instant a row becomes listed (§ created fix).

    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::CommonError(format!(
            "endpoint {} is missing/invalid OpenRouter-required fields: {}",
            row.slug,
            problems.join(", ")
        )))
    }
}

/// Map a listed endpoint row to one OpenRouter `/v1/models` catalog entry.
/// `id` = slug (the callback identity); `openrouter.slug` attaches to the canonical
/// page (always present for a listed row under the attach-or-fail model); `created` =
/// `last_published_at` if published, else `or_listed_at` (listing time), as a Unix
/// epoch; `name` falls back to `"<slug> (InferX)"`. Optional fields are only emitted
/// when set.
pub fn BuildOpenRouterModelEntry(row: &ListedEndpoint) -> Value {
    let name = row
        .or_name
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{} (InferX)", row.slug));

    let created = row
        .last_published_at
        .or(row.or_listed_at)
        .map(|t| t.timestamp())
        .unwrap_or(0);

    let mut entry = serde_json::json!({
        "id": row.slug,
        "hugging_face_id": row.hugging_face_id.clone().unwrap_or_default(),
        "name": name,
        "created": created,
        "input_modalities": row.input_modalities.clone().unwrap_or_default(),
        "output_modalities": row.output_modalities.clone().unwrap_or_default(),
        "quantization": row.quantization.clone().unwrap_or_default(),
        "context_length": row.context_length.unwrap_or(0),
        "max_output_length": row.max_output_length.unwrap_or(0),
        "pricing": row.pricing.clone().unwrap_or(Value::Null),
        "supported_sampling_parameters": row.supported_sampling_parameters.clone().unwrap_or_default(),
        "supported_features": row.supported_features.clone().unwrap_or_default(),
    });

    let obj = entry.as_object_mut().unwrap();
    if let Some(slug) = &row.openrouter_slug {
        if !slug.trim().is_empty() {
            obj.insert("openrouter".into(), serde_json::json!({ "slug": slug }));
        }
    }
    if let Some(d) = row.discount_to_user {
        obj.insert("discount_to_user".into(), serde_json::json!(d));
    }
    if let Some(ready) = row.or_is_ready {
        obj.insert("is_ready".into(), serde_json::json!(ready));
    }
    if let Some(dep) = row.or_deprecation_date {
        obj.insert(
            "deprecation_date".into(),
            serde_json::json!(dep.format("%Y-%m-%d").to_string()),
        );
    }

    entry
}

/// True when `s` has the Hugging Face `author/name` repo-id shape: exactly one `/`,
/// both sides non-empty, and no whitespace. Used to decide whether the func's
/// `--model` value is an HF id worth deriving (vs. a local mount path).
pub fn looks_like_hf_repo_id(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.contains(char::is_whitespace) {
        return false;
    }
    let mut parts = s.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(author), Some(name), None) => !author.is_empty() && !name.is_empty(),
        _ => false,
    }
}

/// Pure join: collect the catalog `id`s whose `hugging_face_id` equals `hf_id`. Split
/// out from the network fetch so the suggestion path is unit-testable.
fn JoinCatalogOnHfId(hf_id: &str, data: &[Value]) -> Vec<String> {
    let mut matches: Vec<String> = Vec::new();
    for entry in data {
        let entry_hf = entry
            .get("hugging_face_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !entry_hf.is_empty() && entry_hf == hf_id {
            if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
                matches.push(id.to_string());
            }
        }
    }
    matches
}

/// Pure admin-edit validation against a fetched catalog (§ Validation Rules). Hard
/// gate: `slug` must match some entry's `id`. A real `id` is accepted; `:free` /
/// HF-mismatch produce non-blocking warnings. Split out from the network fetch so it
/// is unit-testable. `endpoint_hf_id` is the (already-non-empty) endpoint HF id.
fn ValidateSlugInCatalog(
    slug: &str,
    endpoint_hf_id: &str,
    data: &[Value],
) -> Result<SlugValidation> {
    let entry = data
        .iter()
        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(slug))
        .ok_or_else(|| {
            Error::CommonError(format!(
                "openrouter_slug `{}` is not a valid OpenRouter catalog id (no such model page)",
                slug
            ))
        })?;

    let mut warnings: Vec<String> = Vec::new();
    let entry_hf = entry
        .get("hugging_face_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !entry_hf.is_empty() && entry_hf != endpoint_hf_id {
        warnings.push(format!(
            "openrouter_slug `{}` maps to hugging_face_id `{}`, which differs from this endpoint's `{}` — likely the wrong model's page",
            slug, entry_hf, endpoint_hf_id
        ));
    }
    if slug.ends_with(":free") {
        warnings.push(format!(
            "openrouter_slug `{}` is a :free page; InferX is a billed provider, so this is unusual — confirm it is intended",
            slug
        ));
    }

    Ok(SlugValidation { warnings })
}

/// Pure tie-break over the OpenRouter `id`s that joined on a single `hugging_face_id`
/// 0 → None (no suggestion); 1 → that id; >1 → the single paid (non-`:free`)
/// id, else fail closed. Split out from the network fetch so it is unit-testable.
fn ResolveSlugFromMatches(hf_id: &str, matches: Vec<String>) -> Result<Option<String>> {
    // De-dup while preserving the join result.
    let mut unique: Vec<String> = Vec::new();
    for m in matches {
        if !unique.contains(&m) {
            unique.push(m);
        }
    }

    match unique.len() {
        0 => Ok(None),
        1 => Ok(Some(unique.into_iter().next().unwrap())),
        _ => {
            // Auto-pick ONLY when the candidates differ solely by a trailing `:free`
            // on one shared base id (the paid + `:free` variant pair). One paid id
            // plus an *unrelated* `:free` id is genuinely ambiguous → fail closed
            // rather than mis-attaching. Never attach to `:free`.
            let bases: BTreeSet<&str> = unique
                .iter()
                .map(|m| m.strip_suffix(":free").unwrap_or(m))
                .collect();
            let paid: Vec<&String> = unique.iter().filter(|m| !m.ends_with(":free")).collect();
            if bases.len() == 1 && paid.len() == 1 {
                Ok(Some(paid[0].clone()))
            } else {
                Err(Error::CommonError(format!(
                    "OpenRouter slug ambiguous for hugging_face_id {}: {:?}; type a catalog id in the openrouter_slug field to pin one",
                    hf_id, unique
                )))
            }
        }
    }
}

/// Build the prospective listed row from an existing row + an incoming editable
/// metadata draft. The operator-authored fields — now including
/// `context_length` and `openrouter_slug`, which the OpenRouter section edits — come
/// from `meta`; only the listing/audit state (created/listing flags) comes from
/// `existing`. Used to validate that a draft save won't break a live listing, so it
/// MUST validate against the *incoming* edit, not the stale row value.
fn MergeListingMetadata(
    existing: &ListedEndpoint,
    meta: &EndpointOpenRouterMetadata,
) -> ListedEndpoint {
    ListedEndpoint {
        slug: existing.slug.clone(),
        or_name: meta.or_name.clone(),
        hugging_face_id: meta.hugging_face_id.clone(),
        quantization: meta.quantization.clone(),
        input_modalities: meta.input_modalities.clone(),
        output_modalities: meta.output_modalities.clone(),
        context_length: meta.context_length,
        max_output_length: meta.max_output_length,
        pricing: meta.pricing.clone(),
        discount_to_user: meta.discount_to_user,
        supported_sampling_parameters: meta.supported_sampling_parameters.clone(),
        supported_features: meta.supported_features.clone(),
        openrouter_slug: meta.openrouter_slug.clone(),
        or_listed: existing.or_listed,
        or_is_ready: existing.or_is_ready,
        or_deprecation_date: existing.or_deprecation_date,
        last_published_at: existing.last_published_at,
        or_listed_at: existing.or_listed_at,
    }
}

#[cfg(test)]
mod openrouter_tests {
    use super::*;

    fn complete_row() -> ListedEndpoint {
        ListedEndpoint {
            slug: "qwq-32b".into(),
            or_name: Some("Qwen QwQ 32B (InferX)".into()),
            hugging_face_id: Some("Qwen/QwQ-32B".into()),
            quantization: Some("bf16".into()),
            input_modalities: Some(vec!["text".into()]),
            output_modalities: Some(vec!["text".into()]),
            context_length: Some(32768),
            max_output_length: Some(8192),
            pricing: Some(serde_json::json!({"prompt": "0.0000004", "completion": "0.0000012"})),
            discount_to_user: None,
            supported_sampling_parameters: Some(vec!["temperature".into()]),
            supported_features: Some(vec![]),
            openrouter_slug: Some("qwen/qwq-32b".into()),
            or_listed: false,
            or_is_ready: None,
            or_deprecation_date: None,
            last_published_at: Some(chrono::Utc::now()),
            or_listed_at: None,
        }
    }

    #[test]
    fn complete_row_validates() {
        // Empty (but Some) supported_features is a valid "genuinely none" value.
        assert!(ValidateOpenRouterListing(&complete_row()).is_ok());
    }

    #[test]
    fn null_modality_is_rejected() {
        let mut row = complete_row();
        row.input_modalities = None;
        assert!(ValidateOpenRouterListing(&row).is_err());
    }

    #[test]
    fn empty_modality_is_rejected() {
        let mut row = complete_row();
        row.input_modalities = Some(vec![]);
        assert!(ValidateOpenRouterListing(&row).is_err());
    }

    #[test]
    fn missing_pricing_is_rejected() {
        let mut row = complete_row();
        row.pricing = None;
        assert!(ValidateOpenRouterListing(&row).is_err());
    }

    #[test]
    fn pricing_without_completion_is_rejected() {
        let mut row = complete_row();
        row.pricing = Some(serde_json::json!({"prompt": "0.0000004"}));
        assert!(ValidateOpenRouterListing(&row).is_err());
    }

    #[test]
    fn tiered_pricing_is_valid() {
        let mut row = complete_row();
        row.pricing = Some(serde_json::json!([
            {"prompt": "0.0000004", "completion": "0.0000012"},
            {"min_context": 16384, "prompt": "0.0000008", "completion": "0.0000024"}
        ]));
        assert!(ValidateOpenRouterListing(&row).is_ok());
    }

    #[test]
    fn three_pricing_tiers_is_rejected() {
        let mut row = complete_row();
        row.pricing = Some(serde_json::json!([
            {"prompt": "1", "completion": "1"},
            {"prompt": "2", "completion": "2"},
            {"prompt": "3", "completion": "3"}
        ]));
        assert!(ValidateOpenRouterListing(&row).is_err());
    }

    #[test]
    fn slug_resolution_no_match_is_none() {
        // No match → None (no suggestion). The "new page" success path is removed; the
        // list op fails closed on an empty slug rather than listing without one.
        assert_eq!(ResolveSlugFromMatches("hf", vec![]).unwrap(), None);
    }

    #[test]
    fn slug_resolution_single_match() {
        assert_eq!(
            ResolveSlugFromMatches("hf", vec!["qwen/qwq-32b".into()]).unwrap(),
            Some("qwen/qwq-32b".into())
        );
    }

    #[test]
    fn slug_resolution_paid_over_free() {
        let got = ResolveSlugFromMatches(
            "hf",
            vec!["qwen/qwq-32b".into(), "qwen/qwq-32b:free".into()],
        )
        .unwrap();
        assert_eq!(got, Some("qwen/qwq-32b".into()));
    }

    #[test]
    fn slug_resolution_ambiguous_fails_closed() {
        let res =
            ResolveSlugFromMatches("hf", vec!["a/one".into(), "a/two".into()]);
        assert!(res.is_err());
    }

    #[test]
    fn slug_resolution_paid_plus_unrelated_free_fails_closed() {
        // One paid id plus an UNRELATED `:free` id (different base) must NOT
        // auto-pick the paid one — that would mis-attach. Fail closed instead.
        let res = ResolveSlugFromMatches(
            "hf",
            vec!["a/one".into(), "b/two:free".into()],
        );
        assert!(res.is_err());
    }

    #[test]
    fn slug_resolution_dedups_identical_matches() {
        // Duplicate join hits on the same id resolve as a single match.
        assert_eq!(
            ResolveSlugFromMatches("hf", vec!["a/one".into(), "a/one".into()]).unwrap(),
            Some("a/one".into())
        );
    }

    fn complete_meta() -> EndpointOpenRouterMetadata {
        EndpointOpenRouterMetadata {
            or_name: Some("Qwen QwQ 32B (InferX)".into()),
            hugging_face_id: Some("Qwen/QwQ-32B".into()),
            quantization: Some("bf16".into()),
            input_modalities: Some(vec!["text".into()]),
            output_modalities: Some(vec!["text".into()]),
            max_output_length: Some(8192),
            pricing: Some(serde_json::json!({"prompt": "0.0000004", "completion": "0.0000012"})),
            discount_to_user: None,
            supported_sampling_parameters: Some(vec!["temperature".into()]),
            supported_features: Some(vec![]),
            context_length: Some(32768),
            openrouter_slug: Some("qwen/qwq-32b".into()),
        }
    }

    fn catalog() -> Vec<Value> {
        vec![
            serde_json::json!({"id": "qwen/qwq-32b", "hugging_face_id": "Qwen/QwQ-32B"}),
            serde_json::json!({"id": "qwen/qwq-32b:free", "hugging_face_id": "Qwen/QwQ-32B"}),
            serde_json::json!({"id": "meta/llama-3", "hugging_face_id": "meta-llama/Llama-3"}),
            // `canonical_slug`-only entry: present as canonical_slug, NOT as an `id`.
            serde_json::json!({"id": "vendor/real-id", "canonical_slug": "vendor/canonical-only", "hugging_face_id": "vendor/model"}),
        ]
    }

    #[test]
    fn save_guard_blocks_breaking_a_live_listing() {
        // The save-guard logic: a draft that clears a required field on a live
        // listing must be rejected (checklist "save draft incomplete -> List blocked"
        // applied to an already-listed row).
        let mut existing = complete_row();
        existing.or_listed = true;
        let mut meta = complete_meta();
        meta.input_modalities = None;
        let prospective = MergeListingMetadata(&existing, &meta);
        assert!(ValidateOpenRouterListing(&prospective).is_err());
    }

    #[test]
    fn save_guard_allows_valid_edit_on_live_listing() {
        let mut existing = complete_row();
        existing.or_listed = true;
        let mut meta = complete_meta();
        meta.or_name = Some("Renamed (InferX)".into());
        let prospective = MergeListingMetadata(&existing, &meta);
        assert!(ValidateOpenRouterListing(&prospective).is_ok());
    }

    #[test]
    fn merge_takes_editable_fields_from_incoming_edit() {
        // context_length and openrouter_slug are now OR-editable, so the prospective
        // row must take them from the *incoming* edit (meta), not the stale row — else
        // the live-listing guard would validate against a value the Save is changing.
        let mut existing = complete_row();
        existing.or_listed = true;
        existing.context_length = Some(131072);
        existing.openrouter_slug = Some("stale/slug".into());
        let mut meta = complete_meta();
        meta.context_length = Some(8192);
        meta.openrouter_slug = Some("qwen/qwq-32b".into());
        let merged = MergeListingMetadata(&existing, &meta);
        assert_eq!(merged.context_length, Some(8192));
        assert_eq!(merged.openrouter_slug, Some("qwen/qwq-32b".into()));
        // Only listing/audit state comes from the existing row.
        assert!(merged.or_listed);
    }

    #[test]
    fn merge_clearing_context_length_on_live_listing_is_rejected() {
        // The context_length case from the live-listing guard: clearing it via an edit
        // must be caught by validating the prospective (incoming) row.
        let mut existing = complete_row();
        existing.or_listed = true;
        let mut meta = complete_meta();
        meta.context_length = None;
        let prospective = MergeListingMetadata(&existing, &meta);
        assert!(ValidateOpenRouterListing(&prospective).is_err());
    }

    #[test]
    fn validate_listing_requires_hugging_face_id() {
        let mut row = complete_row();
        row.hugging_face_id = None;
        assert!(ValidateOpenRouterListing(&row).is_err());
    }

    #[test]
    fn validate_listing_no_longer_requires_last_published_at() {
        // created falls back to or_listed_at; an unpublished listing is valid.
        let mut row = complete_row();
        row.last_published_at = None;
        row.or_listed_at = Some(chrono::Utc::now());
        assert!(ValidateOpenRouterListing(&row).is_ok());
    }

    #[test]
    fn created_falls_back_to_listing_time_when_unpublished() {
        let mut row = complete_row();
        row.last_published_at = None;
        row.or_listed_at = Some(chrono::Utc::now());
        let entry = BuildOpenRouterModelEntry(&row);
        assert!(entry["created"].as_i64().unwrap() > 0);
    }

    #[test]
    fn validate_slug_accepts_real_id_with_matching_hf() {
        let v = ValidateSlugInCatalog("qwen/qwq-32b", "Qwen/QwQ-32B", &catalog()).unwrap();
        assert!(v.warnings.is_empty());
    }

    #[test]
    fn validate_slug_rejects_nonexistent() {
        assert!(ValidateSlugInCatalog("no/such-id", "Qwen/QwQ-32B", &catalog()).is_err());
    }

    #[test]
    fn validate_slug_rejects_canonical_slug_only() {
        // A string that exists only as a `canonical_slug`, not as an `id`, is rejected.
        assert!(ValidateSlugInCatalog("vendor/canonical-only", "vendor/model", &catalog()).is_err());
    }

    #[test]
    fn validate_slug_warns_on_hf_mismatch() {
        let v = ValidateSlugInCatalog("meta/llama-3", "Qwen/QwQ-32B", &catalog()).unwrap();
        assert_eq!(v.warnings.len(), 1);
        assert!(v.warnings[0].contains("differs"));
    }

    #[test]
    fn validate_slug_warns_on_free_page() {
        let v = ValidateSlugInCatalog("qwen/qwq-32b:free", "Qwen/QwQ-32B", &catalog()).unwrap();
        assert!(v.warnings.iter().any(|w| w.contains(":free")));
    }

    #[test]
    fn suggest_join_single_match() {
        let matches = JoinCatalogOnHfId("meta-llama/Llama-3", &catalog());
        assert_eq!(
            ResolveSlugFromMatches("hf", matches).unwrap(),
            Some("meta/llama-3".into())
        );
    }

    #[test]
    fn suggest_join_paid_over_free() {
        let matches = JoinCatalogOnHfId("Qwen/QwQ-32B", &catalog());
        assert_eq!(
            ResolveSlugFromMatches("hf", matches).unwrap(),
            Some("qwen/qwq-32b".into())
        );
    }

    #[test]
    fn suggest_join_no_match_is_none() {
        let matches = JoinCatalogOnHfId("nobody/nothing", &catalog());
        assert_eq!(ResolveSlugFromMatches("hf", matches).unwrap(), None);
    }

    #[test]
    fn hf_repo_id_shape() {
        assert!(looks_like_hf_repo_id("Qwen/QwQ-32B"));
        assert!(looks_like_hf_repo_id("meta-llama/Llama-3.1-8B"));
        assert!(!looks_like_hf_repo_id("/models/local/path/weights")); // local mount
        assert!(!looks_like_hf_repo_id("just-a-name"));
        assert!(!looks_like_hf_repo_id("a/b/c"));
        assert!(!looks_like_hf_repo_id("has space/name"));
        assert!(!looks_like_hf_repo_id(""));
    }

    #[test]
    fn model_entry_shape() {
        let mut row = complete_row();
        row.openrouter_slug = Some("qwen/qwq-32b".into());
        let entry = BuildOpenRouterModelEntry(&row);
        assert_eq!(entry["id"], serde_json::json!("qwq-32b"));
        assert_eq!(entry["openrouter"]["slug"], serde_json::json!("qwen/qwq-32b"));
        assert_eq!(entry["hugging_face_id"], serde_json::json!("Qwen/QwQ-32B"));
        assert!(entry["created"].as_i64().unwrap() > 0);
    }

    #[test]
    fn model_entry_name_fallback() {
        let mut row = complete_row();
        row.or_name = None;
        let entry = BuildOpenRouterModelEntry(&row);
        assert_eq!(entry["name"], serde_json::json!("qwq-32b (InferX)"));
        // A listed row always carries openrouter.slug under the attach-or-fail model.
        assert_eq!(entry["openrouter"]["slug"], serde_json::json!("qwen/qwq-32b"));
    }
}
