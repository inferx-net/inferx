use std::{
    collections::{BTreeSet, HashMap},
    result::Result as SResult,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio::sync::OnceCell;

use axum_keycloak_auth::{
    decode::{KeycloakToken, ProfileAndEmail},
    instance::{KeycloakAuthInstance, KeycloakConfig},
    layer::KeycloakAuthLayer,
    KeycloakAuthStatus, PassthroughMode, Url,
};

use keycloak::KeycloakAdmin;
use keycloak::KeycloakAdminToken;
use std::ops::Bound::Included;
use std::ops::Bound::Unbounded;

use super::secret::{Apikey, SqlSecret};
use crate::gateway::http_gateway::GATEWAY_CONFIG;
use crate::common::*;

pub const SECRET_ADDR: &str = "postgresql://audit_user:123456@localhost:5431/secretdb";

static TOKEN_CACHE: OnceCell<TokenCache> = OnceCell::const_new();

pub async fn GetTokenCache() -> &'static TokenCache {
    TOKEN_CACHE
        .get_or_init(|| async {
            info!(
                "GetTokenCache config {:?} secretstore addr {}",
                &GATEWAY_CONFIG.keycloakconfig, &GATEWAY_CONFIG.secretStoreAddr
            );
            match TokenCache::New(
                &GATEWAY_CONFIG.secretStoreAddr,
                &GATEWAY_CONFIG.keycloakconfig,
            )
            .await
            {
                Err(e) => {
                    error!("GetTokenCache error {:?}", e);
                    panic!();
                }
                Ok(t) => t,
            }
        })
        .await
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct Permision {
    pub admin: bool,
    pub user: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct KeycloadConfig {
    pub url: String,
    pub realm: String,
}

impl KeycloadConfig {
    pub fn AuthLayer(&self) -> KeycloakAuthLayer<String> {
        // Initialize Keycloak authentication instance
        let auth_instance = KeycloakAuthInstance::new(
            KeycloakConfig::builder()
                .server(Url::parse(&self.url).unwrap())
                .realm(String::from(&self.realm))
                .build(),
        );

        // Wrap the instance in an Arc for shared ownership
        let auth_instance = Arc::new(auth_instance);

        let auth_layer = KeycloakAuthLayer::<String>::builder()
            .instance(auth_instance)
            .passthrough_mode(PassthroughMode::Pass)
            .persist_raw_claims(false)
            .expected_audiences(vec![String::from("account")])
            // .required_roles(vec![String::from("administrator")])
            .build();

        return auth_layer;
    }
}

pub fn Realm(token: &KeycloakToken<String>) -> String {
    let issuer = &token.issuer;
    let splits: Vec<&str> = issuer.split("/").collect();
    return splits[splits.len() - 1].to_owned();
}

pub fn Username(token: &KeycloakToken<String>) -> String {
    return token.extra.profile.preferred_username.clone();
}

pub fn DisplayName(token: &KeycloakToken<String>) -> Option<String> {
    return token
        .extra
        .profile
        .full_name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            token
                .extra
                .profile
                .given_name
                .clone()
                .filter(|s| !s.trim().is_empty())
        });
}

pub fn Subject(token: &KeycloakToken<String>) -> String {
    return token.subject.clone();
}

pub fn Email(token: &KeycloakToken<String>) -> String {
    return token.extra.email.email.clone();
}

pub fn EmailVerified(token: &KeycloakToken<String>) -> bool {
    return token.extra.email.email_verified;
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")] // This matches "grant" in URL to PermissionType::Grant
pub enum PermissionType {
    Grant,
    Revoke,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ObjectType {
    Tenant,
    Namespace,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    User,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Grant {
    pub objType: ObjectType,
    pub tenant: String,
    pub namespace: String,
    pub name: String,
    pub role: UserRole,
    pub username: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RoleBinding {
    pub objType: ObjectType,
    pub role: UserRole,
    pub tenant: String,
    pub namespace: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ApikeyCreateRequest {
    #[serde(default)]
    pub username: String,
    pub keyname: String,
    #[serde(default)]
    pub access_level: Option<String>,
    #[serde(default)]
    pub restrict_tenant: Option<String>,
    #[serde(default)]
    pub restrict_namespace: Option<String>,
    #[serde(default)]
    pub expires_in_days: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ApikeyCreateResponse {
    pub apikey: String,
    pub keyname: String,
    pub access_level: String,
    pub restrict_tenant: Option<String>,
    pub restrict_namespace: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ApikeyDeleteRequest {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub keyname: String,
}

#[derive(Debug, Clone)]
pub struct AccessToken {
    pub subject: String,
    pub username: String,
    pub display_name: Option<String>,
    pub email: String,
    pub email_verified: bool,
    pub roles: BTreeSet<String>,
    pub apiKeys: Vec<String>,
    pub scope: String,
    pub sourceIsApikey: bool,
    pub restrictTenant: Option<String>,
    pub restrictNamespace: Option<String>,
    pub updatetime: SystemTime,
}

impl AccessToken {
    pub const INERX_ADMIN: &str = "inferx_admin";
    pub const SYSTEM_TENANT: &str = "system";
    pub const TENANT_ADMIN: &str = "/tenant/admin/";
    pub const TENANT_USER: &str = "/tenant/user/";
    pub const NAMSPACE_ADMIN: &str = "/namespace/admin/";
    pub const NAMSPACE_USER: &str = "/namespace/user/";

    pub fn New(username: &str, groups: Vec<String>, apikeys: Vec<String>) -> Self {
        let mut set = BTreeSet::new();

        for g in groups {
            set.insert(g);
        }

        return Self {
            subject: "".to_owned(),
            username: username.to_owned(),
            display_name: None,
            email: "".to_owned(),
            email_verified: false,
            roles: set,
            apiKeys: apikeys,
            scope: "full".to_owned(),
            sourceIsApikey: false,
            restrictTenant: None,
            restrictNamespace: None,
            updatetime: SystemTime::now(),
        };
    }

    pub fn InferxAdminToken() -> Self {
        return Self {
            subject: "".to_owned(),
            username: Self::INERX_ADMIN.to_owned(),
            display_name: None,
            email: "".to_owned(),
            email_verified: false,
            roles: BTreeSet::new(),
            apiKeys: Vec::new(),
            scope: "full".to_owned(),
            sourceIsApikey: false,
            restrictTenant: None,
            restrictNamespace: None,
            updatetime: SystemTime::now(),
        };
    }

    fn ScopeLevel(scope: &str) -> i32 {
        match scope {
            "full" => 3,
            "read" => 2,
            "inference" => 1,
            _ => 3, // Backward-compatible default for existing tokens
        }
    }

    pub fn CheckScope(&self, required: &str) -> bool {
        return Self::ScopeLevel(&self.scope) >= Self::ScopeLevel(required);
    }

    fn HasInferxAdminIdentity(&self) -> bool {
        let prefix = Self::TENANT_ADMIN;
        let systemtenant = Self::SYSTEM_TENANT;
        let isSystemAdmin = self.roles.contains(&format!("{prefix}{systemtenant}"));
        return &self.username == Self::INERX_ADMIN || isSystemAdmin;
    }

    fn IsSystemScopedInferxAdminApikey(&self) -> bool {
        return self.sourceIsApikey
            && self.CheckScope("full")
            && self.restrictTenant.as_deref() == Some(Self::SYSTEM_TENANT)
            && self.restrictNamespace.is_none()
            && self.HasInferxAdminIdentity();
    }

    fn AllowTenant(&self, tenant: &str) -> bool {
        if self.IsSystemScopedInferxAdminApikey() {
            return true;
        }

        match &self.restrictTenant {
            Some(t) => t == tenant,
            None => true,
        }
    }

    fn AllowNamespace(&self, tenant: &str, namespace: &str) -> bool {
        if !self.AllowTenant(tenant) {
            return false;
        }

        match &self.restrictNamespace {
            Some(ns) => ns == namespace,
            None => true,
        }
    }

    fn HasTenantAdminRole(&self, tenant: &str) -> bool {
        let prefix = Self::TENANT_ADMIN;
        return self.roles.contains(&format!("{prefix}{tenant}"));
    }

    fn HasTenantUserRole(&self, tenant: &str) -> bool {
        let prefix = Self::TENANT_USER;
        return self.roles.contains(&format!("{prefix}{tenant}"));
    }

    fn HasNamespaceAdminRole(&self, tenant: &str, namespace: &str) -> bool {
        let prefix = Self::NAMSPACE_ADMIN;
        return self
            .roles
            .contains(&format!("{prefix}{tenant}/{namespace}"));
    }

    fn HasNamespaceUserRole(&self, tenant: &str, namespace: &str) -> bool {
        let prefix = Self::NAMSPACE_USER;
        return self
            .roles
            .contains(&format!("{prefix}{tenant}/{namespace}"));
    }

    fn HasTenantUserPermission(&self, tenant: &str) -> bool {
        if self.IsInferxAdmin() {
            return true;
        }

        if tenant == "public" {
            return true;
        }

        return self.HasTenantUserRole(tenant) || self.HasTenantAdminRole(tenant);
    }

    fn HasNamespaceAdminPermission(&self, tenant: &str, namespace: &str) -> bool {
        if self.IsInferxAdmin() {
            return true;
        }

        return self.HasTenantAdminRole(tenant) || self.HasNamespaceAdminRole(tenant, namespace);
    }

    fn HasNamespaceUserPermission(&self, tenant: &str, namespace: &str) -> bool {
        if self.IsInferxAdmin() {
            return true;
        }

        if tenant == "public" {
            return true;
        }

        if self.HasTenantUserPermission(tenant) {
            return true;
        }

        return self.HasNamespaceUserRole(tenant, namespace)
            || self.HasNamespaceAdminPermission(tenant, namespace);
    }

    pub fn IsInferxAdmin(&self) -> bool {
        if self.sourceIsApikey {
            if !self.CheckScope("full") {
                return false;
            }
            if (self.restrictTenant.is_some() || self.restrictNamespace.is_some())
                && !self.IsSystemScopedInferxAdminApikey()
            {
                return false;
            }
        }

        return self.HasInferxAdminIdentity();
    }

    pub fn UserKey(&self) -> String {
        return format!("{}", &self.username);
    }

    pub fn Timeout(&self) -> bool {
        match SystemTime::now().duration_since(self.updatetime) {
            Err(_e) => return true,
            Ok(d) => {
                // 60 sec cache time
                if d <= Duration::from_secs(60) {
                    return false;
                }
                return true;
            }
        }
    }

    pub fn TenantUserRole(tenant: &str) -> String {
        let prefix = Self::TENANT_USER;
        return format!("{prefix}{tenant}");
    }

    pub fn TenantAdminRole(tenant: &str) -> String {
        let prefix = Self::TENANT_ADMIN;
        return format!("{prefix}{tenant}");
    }

    pub fn NamespaceUserRole(tenant: &str, namespace: &str) -> String {
        let prefix = Self::NAMSPACE_USER;
        return format!("{prefix}{tenant}/{namespace}");
    }

    pub fn NamespaceAdminRole(tenant: &str, namespace: &str) -> String {
        let prefix = Self::NAMSPACE_ADMIN;
        return format!("{prefix}{tenant}/{namespace}");
    }

    pub fn IsTenantUser(&self, tenant: &str) -> bool {
        if !self.CheckScope("read") {
            return false;
        }

        if !self.AllowTenant(tenant) {
            return false;
        }

        return self.HasTenantUserPermission(tenant);
    }

    pub fn IsNamespaceUser(&self, tenant: &str, namespace: &str) -> bool {
        if !self.CheckScope("read") {
            return false;
        }

        if !self.AllowNamespace(tenant, namespace) {
            return false;
        }

        return self.HasNamespaceUserPermission(tenant, namespace);
    }

    pub fn IsNamespaceInferenceUser(&self, tenant: &str, namespace: &str) -> bool {
        if !self.CheckScope("inference") {
            return false;
        }

        if !self.AllowNamespace(tenant, namespace) {
            return false;
        }

        return self.HasNamespaceUserPermission(tenant, namespace);
    }

    pub fn IsTenantAdmin(&self, tenant: &str) -> bool {
        if !self.CheckScope("full") {
            return false;
        }

        if !self.AllowTenant(tenant) {
            return false;
        }

        if self.IsInferxAdmin() {
            return true;
        }

        return self.HasTenantAdminRole(tenant);
    }

    pub fn IsNamespaceAdmin(&self, tenant: &str, namespace: &str) -> bool {
        if !self.CheckScope("full") {
            return false;
        }

        if !self.AllowNamespace(tenant, namespace) {
            return false;
        }

        return self.HasNamespaceAdminPermission(tenant, namespace);
    }

    pub fn AdminTenants(&self) -> Vec<String> {
        let mut items = Vec::new();
        let prefix = Self::TENANT_ADMIN;
        for elem in self.roles.range::<str, _>((Included(prefix), Unbounded)) {
            match elem.strip_prefix(prefix) {
                None => break,
                Some(tenant) => {
                    if self.AllowTenant(tenant) {
                        items.push(tenant.to_owned());
                    }
                }
            }
        }

        return items;
    }

    pub fn AdminNamespaces(&self) -> Vec<(String, String)> {
        let mut items = Vec::new();
        let prefix = Self::NAMSPACE_ADMIN;
        for elem in self.roles.range::<str, _>((Included(prefix), Unbounded)) {
            match elem.strip_prefix(prefix) {
                None => break,
                Some(s) => {
                    let splits: Vec<&str> = s.split("/").collect();
                    assert!(splits.len() == 2);
                    if self.AllowNamespace(splits[0], splits[1]) {
                        items.push((splits[0].to_owned(), splits[1].to_owned()));
                    }
                }
            }
        }

        return items;
    }

    pub fn UserTenants(&self) -> Vec<String> {
        let mut items = BTreeSet::new();
        let prefix = Self::TENANT_USER;
        for elem in self.roles.range::<str, _>((Included(prefix), Unbounded)) {
            match elem.strip_prefix(prefix) {
                None => break,
                Some(tenant) => {
                    if self.AllowTenant(tenant) {
                        items.insert(tenant.to_owned());
                    }
                }
            }
        }

        for e in self.AdminTenants() {
            items.insert(e);
        }

        let mut v = Vec::new();
        for i in items {
            v.push(i);
        }

        // anyone has public tenant user permission
        if self.AllowTenant("public") {
            v.push("public".to_owned());
        }

        return v;
    }

    pub fn UserNamespaces(&self) -> Vec<(String, String)> {
        let mut items = BTreeSet::new();
        let prefix = Self::NAMSPACE_USER;
        for elem in self.roles.range::<str, _>((Included(prefix), Unbounded)) {
            match elem.strip_prefix(prefix) {
                None => break,
                Some(s) => {
                    let splits: Vec<&str> = s.split("/").collect();
                    assert!(splits.len() == 2);
                    if self.AllowNamespace(splits[0], splits[1]) {
                        items.insert((splits[0].to_owned(), splits[1].to_owned()));
                    }
                }
            }
        }

        for e in self.AdminNamespaces() {
            items.insert(e);
        }

        let mut v = Vec::new();
        for i in items {
            v.push(i);
        }

        return v;
    }

    pub fn RoleBindings(&self) -> Result<Vec<RoleBinding>> {
        let mut bindings = Vec::new();
        for r in &self.roles {
            let binding = ParseRoleBinding(r)?;
            bindings.push(binding);
        }

        return Ok(bindings);
    }
}

fn ParseRoleBinding(input: &str) -> Result<RoleBinding> {
    let parts: Vec<&str> = input.trim_matches('/').split('/').collect();

    if parts.len() != 3 && parts.len() != 4 {
        return Err(Error::CommonError(format!(
            "invalid ParseRoleBinding 1 path: {}",
            input
        )));
    }

    let role = match parts[1] {
        "admin" => UserRole::Admin,
        "user" => UserRole::User,
        _ => {
            return Err(Error::CommonError(format!(
                "unknown role: {} in {:?}",
                parts[1], parts
            )))
        }
    };

    let objType = match parts[0] {
        "tenant" => ObjectType::Tenant,
        "namespace" => ObjectType::Namespace,
        _ => {
            return Err(Error::CommonError(format!(
                "unknown scope: {} in {:?}",
                parts[0], parts
            )))
        }
    };

    match objType {
        ObjectType::Tenant => {
            if parts.len() != 3 {
                return Err(Error::CommonError(format!(
                    "invalid ParseRoleBinding 2 path: {:?}",
                    parts
                )));
            }
            Ok(RoleBinding {
                objType: objType,
                role: role,
                tenant: parts[2].to_string(),
                namespace: String::new(),
            })
        }
        ObjectType::Namespace => {
            if parts.len() != 4 {
                return Err(Error::CommonError(format!(
                    "invalid ParseRoleBinding 3 path: {:?}",
                    parts
                )));
            }
            Ok(RoleBinding {
                objType: objType,
                role: role,
                tenant: parts[2].to_string(),
                namespace: parts[3].to_string(),
            })
        }
    }
}

#[derive(Debug)]
pub struct TokenCache {
    pub apikeyStore: Mutex<HashMap<String, Arc<AccessToken>>>,
    pub usernameStore: Mutex<HashMap<String, Arc<AccessToken>>>,
    pub sqlstore: SqlSecret,
    pub updatelock: Mutex<()>,
    pub anonymous: Arc<AccessToken>,
}

impl TokenCache {
    pub async fn New(secretSqlUrl: &str, _keycloakConfig: &KeycloadConfig) -> Result<Self> {
        let sqlstore = SqlSecret::New(secretSqlUrl).await?;
        return Ok(Self {
            apikeyStore: Mutex::new(HashMap::new()),
            usernameStore: Mutex::new(HashMap::new()),
            sqlstore: sqlstore,
            updatelock: Mutex::new(()),
            anonymous: Arc::new(AccessToken {
                subject: "".to_owned(),
                username: "anonymous".to_owned(),
                display_name: None,
                email: "".to_owned(),
                email_verified: false,
                roles: BTreeSet::new(),
                apiKeys: Vec::new(),
                scope: "read".to_owned(),
                sourceIsApikey: false,
                restrictTenant: None,
                restrictNamespace: None,
                updatetime: SystemTime::now(),
            }),
        });
    }

    pub fn Replace(&self, oldToken: Option<Arc<AccessToken>>, newtoken: &Arc<AccessToken>) {
        let _l = self.updatelock.lock().unwrap();
        match oldToken {
            None => (),
            Some(oldToken) => {
                for k in &oldToken.apiKeys {
                    self.apikeyStore.lock().unwrap().remove(k);
                }

                self.usernameStore
                    .lock()
                    .unwrap()
                    .remove(&oldToken.UserKey());
            }
        }
        self.usernameStore
            .lock()
            .unwrap()
            .insert(newtoken.UserKey(), newtoken.clone());
    }

    pub fn EvactionToken(&self, token: &Arc<AccessToken>) {
        let _l = self.updatelock.lock().unwrap();
        for k in &token.apiKeys {
            self.apikeyStore.lock().unwrap().remove(k);
        }

        self.usernameStore.lock().unwrap().remove(&token.UserKey());
    }

    fn NewRawApikey() -> String {
        let random_bytes: [u8; 32] = rand::random();
        return format!("ix_{}", hex::encode(random_bytes));
    }

    fn NormalizeScope(scope: &str) -> String {
        return scope.to_owned();
    }

    fn ValidateScope(scope: &str) -> Result<()> {
        match scope {
            "full" | "inference" | "read" => Ok(()),
            _ => Err(Error::CommonError(format!(
                "invalid apikey access_level {}",
                scope
            ))),
        }
    }

    fn NormalizeRequiredTenant(restrict_tenant: &Option<String>) -> Result<String> {
        let tenant = match restrict_tenant {
            Some(t) => t.trim(),
            None => return Err(Error::CommonError(format!("restrict_tenant is required"))),
        };

        if tenant.is_empty() {
            return Err(Error::CommonError(format!(
                "restrict_tenant cannot be empty"
            )));
        }

        return Ok(tenant.to_owned());
    }

    fn NormalizeOptionalNamespace(restrict_namespace: &Option<String>) -> Result<Option<String>> {
        match restrict_namespace {
            Some(ns) => {
                let normalized = ns.trim();
                if normalized.is_empty() {
                    return Err(Error::CommonError(format!(
                        "restrict_namespace cannot be empty"
                    )));
                }
                Ok(Some(normalized.to_owned()))
            }
            None => Ok(None),
        }
    }

    fn RoleAllowedByRestriction(
        role: &str,
        restrict_tenant: &str,
        restrict_namespace: Option<&str>,
    ) -> bool {
        let parts: Vec<&str> = role.trim_matches('/').split('/').collect();

        match parts.as_slice() {
            // /tenant/{admin|user}/{tenant}
            ["tenant", _, tenant] => {
                if *tenant != restrict_tenant {
                    return false;
                }
                // Namespace-restricted keys cannot retain tenant-level roles.
                if restrict_namespace.is_some() {
                    return false;
                }
                true
            }
            // /namespace/{admin|user}/{tenant}/{namespace}
            ["namespace", _, tenant, namespace] => {
                if *tenant != restrict_tenant {
                    return false;
                }
                if let Some(rn) = restrict_namespace {
                    if *namespace != rn {
                        return false;
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn ApplyApikeyRestrictions(
        &self,
        base: &Arc<AccessToken>,
        apikey: &Apikey,
    ) -> Result<AccessToken> {
        let normalized_scope = Self::NormalizeScope(&apikey.access_level);
        Self::ValidateScope(&normalized_scope)?;
        let restrict_tenant = Self::NormalizeRequiredTenant(&apikey.restrict_tenant)?;
        let restrict_namespace = Self::NormalizeOptionalNamespace(&apikey.restrict_namespace)?;

        let mut filtered_roles = BTreeSet::new();
        for r in &base.roles {
            if Self::RoleAllowedByRestriction(r, &restrict_tenant, restrict_namespace.as_deref()) {
                filtered_roles.insert(r.clone());
            }
        }

        Ok(AccessToken {
            subject: base.subject.clone(),
            username: base.username.clone(),
            display_name: base.display_name.clone(),
            email: base.email.clone(),
            email_verified: base.email_verified,
            roles: filtered_roles,
            apiKeys: base.apiKeys.clone(),
            scope: normalized_scope,
            sourceIsApikey: true,
            restrictTenant: Some(restrict_tenant),
            restrictNamespace: restrict_namespace,
            updatetime: SystemTime::now(),
        })
    }

    pub async fn GetTokenByApikey(&self, apikey: &str) -> Result<Arc<AccessToken>> {
        if GATEWAY_CONFIG.inferxAdminApikey.len() > 0 && apikey == &GATEWAY_CONFIG.inferxAdminApikey
        {
            return Ok(Arc::new(AccessToken::InferxAdminToken()));
        }

        let cache_key = apikey.to_owned();

        let oldToken = match self.apikeyStore.lock().unwrap().get(&cache_key) {
            Some(g) => {
                if g.Timeout() {
                    Some(g.clone())
                } else {
                    return Ok(g.clone());
                }
            }
            None => None,
        };

        // Drop stale entry and rebuild.
        if oldToken.is_some() {
            self.apikeyStore.lock().unwrap().remove(&cache_key);
        }

        let key = self.sqlstore.GetApikey(&cache_key).await?;

        if key.revoked_at.is_some() {
            return Err(Error::NoPermission);
        }

        if let Some(expires_at) = key.expires_at {
            if chrono::Utc::now().naive_utc() > expires_at {
                return Err(Error::NoPermission);
            }
        }

        let base_token = self.GetTokenByUsername(&key.username).await?;
        let restricted = Arc::new(self.ApplyApikeyRestrictions(&base_token, &key)?);
        self.apikeyStore
            .lock()
            .unwrap()
            .insert(cache_key, restricted.clone());
        return Ok(restricted);
    }

    pub async fn GetTokenByUsername(&self, username: &str) -> Result<Arc<AccessToken>> {
        let oldToken = match self.usernameStore.lock().unwrap().get(username) {
            Some(g) => {
                if g.Timeout() {
                    Some(g.clone())
                } else {
                    return Ok(g.clone());
                }
            }
            None => None,
        };

        let apikeys = self.sqlstore.GetApikeys(username).await?;
        let mut keys = Vec::new();
        for u in apikeys {
            keys.push(u.apikey.clone());
        }

        let groups = self.sqlstore.GetRoles(username).await?;
        let newToken = Arc::new(AccessToken::New(username, groups, keys));

        self.Replace(oldToken, &newToken);
        return Ok(newToken);
    }

    pub async fn CreateApikey(
        &self,
        token: &Arc<AccessToken>,
        req: &ApikeyCreateRequest,
    ) -> Result<ApikeyCreateResponse> {
        let username = if req.username.len() == 0 {
            token.username.clone()
        } else if req.username == token.username {
            token.username.clone()
        } else {
            if !token.IsInferxAdmin() {
                return Err(Error::NoPermission);
            }
            req.username.clone()
        };

        if req.keyname.trim().is_empty() {
            return Err(Error::CommonError(format!(
                "apikey keyname cannot be empty"
            )));
        }
        let is_inferx_admin_self = token.IsInferxAdmin() && username == token.username;
        let (access_level, restrict_tenant, restrict_namespace) = if is_inferx_admin_self {
            if let Some(req_tenant) = &req.restrict_tenant {
                if req_tenant.trim() != AccessToken::SYSTEM_TENANT {
                    return Err(Error::CommonError(format!(
                        "inferx admin self apikey restrict_tenant must be '{}'",
                        AccessToken::SYSTEM_TENANT
                    )));
                }
            }
            if let Some(req_namespace) = &req.restrict_namespace {
                if !req_namespace.trim().is_empty() {
                    return Err(Error::CommonError(format!(
                        "inferx admin self apikey does not allow restrict_namespace"
                    )));
                }
            }
            if let Some(req_access_level) = &req.access_level {
                if req_access_level.trim() != "full" {
                    return Err(Error::CommonError(format!(
                        "inferx admin self apikey access_level must be full"
                    )));
                }
            }
            (
                "full".to_owned(),
                AccessToken::SYSTEM_TENANT.to_owned(),
                None,
            )
        } else {
            let access_level =
                Self::NormalizeScope(req.access_level.as_deref().unwrap_or("inference"));
            Self::ValidateScope(&access_level)?;
            let restrict_tenant = Self::NormalizeRequiredTenant(&req.restrict_tenant)?;
            let restrict_namespace = Self::NormalizeOptionalNamespace(&req.restrict_namespace)?;

            if !token.IsInferxAdmin() {
                match &restrict_namespace {
                    // Namespace-scoped key: allow either tenant-level permission
                    // or namespace-level permission for that exact namespace.
                    Some(ns) => {
                        if !token.IsTenantUser(&restrict_tenant)
                            && !token.IsNamespaceUser(&restrict_tenant, ns)
                        {
                            return Err(Error::NoPermission);
                        }
                    }
                    // Tenant-scoped key: require tenant-level permission.
                    None => {
                        if !token.IsTenantUser(&restrict_tenant) {
                            return Err(Error::NoPermission);
                        }
                    }
                }
            }
            (access_level, restrict_tenant, restrict_namespace)
        };

        let expires_at = req
            .expires_in_days
            .map(|d| chrono::Utc::now().naive_utc() + chrono::Duration::days(d as i64));

        let raw_apikey = Self::NewRawApikey();

        self.sqlstore
            .CreateApikey(&Apikey {
                key_id: 0,
                apikey: raw_apikey.clone(),
                username: username.clone(),
                keyname: req.keyname.clone(),
                access_level: access_level.clone(),
                restrict_tenant: Some(restrict_tenant.clone()),
                restrict_namespace: restrict_namespace.clone(),
                createtime: None,
                expires_at,
                revoked_at: None,
                revoked_by: None,
                revoke_reason: None,
            })
            .await?;

        // Ensure new key list is visible immediately.
        self.usernameStore.lock().unwrap().remove(&username);

        return Ok(ApikeyCreateResponse {
            apikey: raw_apikey,
            keyname: req.keyname.clone(),
            access_level,
            restrict_tenant: Some(restrict_tenant),
            restrict_namespace,
            expires_at: expires_at.map(|v| v.to_string()),
        });
    }

    pub async fn DeleteApiKey(
        &self,
        token: &Arc<AccessToken>,
        req: &ApikeyDeleteRequest,
    ) -> Result<bool> {
        let username = if req.username.len() == 0 {
            token.username.clone()
        } else if req.username == token.username {
            token.username.clone()
        } else {
            if !token.IsInferxAdmin() {
                return Err(Error::NoPermission);
            }
            req.username.clone()
        };

        let deleted_keys = self.sqlstore.DeleteApikey(&req.keyname, &username).await?;
        if deleted_keys.is_empty() {
            return Ok(false);
        }

        {
            let mut store = self.apikeyStore.lock().unwrap();
            for key in &deleted_keys {
                store.remove(key);
            }
        }
        self.usernameStore.lock().unwrap().remove(&username);
        return Ok(true);
    }

    pub async fn GetApikeys(&self, username: &str) -> Result<Vec<Apikey>> {
        return self.sqlstore.GetApikeys(username).await;
    }

    pub async fn GrantTenantAdminPermission(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        username: &str,
    ) -> Result<()> {
        self.sqlstore
            .AddRole(username, &AccessToken::TenantAdminRole(tenant))
            .await?;
        self.EvactionToken(token);
        return Ok(());
    }

    pub async fn RevokeTenantAdminPermission(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        username: &str,
    ) -> Result<()> {
        self.sqlstore
            .DeleteRole(username, &AccessToken::TenantAdminRole(tenant))
            .await?;
        self.EvactionToken(token);
        return Ok(());
    }

    pub async fn GrantTenantUserPermission(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        username: &str,
    ) -> Result<()> {
        self.sqlstore
            .AddRole(username, &AccessToken::TenantUserRole(tenant))
            .await?;
        self.EvactionToken(token);
        return Ok(());
    }

    pub async fn RevokeTenantUserPermission(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        username: &str,
    ) -> Result<()> {
        self.sqlstore
            .DeleteRole(username, &AccessToken::TenantUserRole(tenant))
            .await?;
        self.EvactionToken(token);
        return Ok(());
    }

    pub async fn GrantNamespaceAdminPermission(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        username: &str,
    ) -> Result<()> {
        self.sqlstore
            .AddRole(
                username,
                &&AccessToken::NamespaceAdminRole(tenant, namespace),
            )
            .await?;
        self.EvactionToken(token);
        return Ok(());
    }

    pub async fn RevokeNamespaceAdminPermission(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        username: &str,
    ) -> Result<()> {
        self.sqlstore
            .DeleteRole(
                username,
                &AccessToken::NamespaceAdminRole(tenant, namespace),
            )
            .await?;
        self.EvactionToken(token);
        return Ok(());
    }

    pub async fn GrantNamespaceUserPermission(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        username: &str,
    ) -> Result<()> {
        self.sqlstore
            .AddRole(
                username,
                &&AccessToken::NamespaceUserRole(tenant, namespace),
            )
            .await?;
        self.EvactionToken(token);
        return Ok(());
    }

    pub async fn RevokeNamespaceUserPermission(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
        username: &str,
    ) -> Result<()> {
        self.sqlstore
            .DeleteRole(username, &AccessToken::NamespaceUserRole(tenant, namespace))
            .await?;
        self.EvactionToken(token);
        return Ok(());
    }

    pub async fn GetTenantAdmins(&self, tenant: &str) -> Result<Vec<String>> {
        return self.sqlstore.GetTenantAdmins(tenant).await;
    }

    pub async fn GetTenantUsers(&self, tenant: &str) -> Result<Vec<String>> {
        return self.sqlstore.GetTenantUsers(tenant).await;
    }

    pub async fn GetNamespaceAdmins(&self, tenant: &str, namespace: &str) -> Result<Vec<String>> {
        return self.sqlstore.GetNamespaceAdmins(tenant, namespace).await;
    }

    pub async fn GetNamespaceUsers(&self, tenant: &str, namespace: &str) -> Result<Vec<String>> {
        return self.sqlstore.GetNamespaceUsers(tenant, namespace).await;
    }
}

pub async fn auth_transform_keycloaktoken(
    mut req: Request<Body>,
    next: Next,
) -> SResult<Response, StatusCode> {
    let token: Arc<AccessToken> = if let Some(token) = req
        .extensions()
        .get::<KeycloakAuthStatus<String, ProfileAndEmail>>()
    {
        // error!(
        //     "auth_transform_keycloaktoken headers is {:#?}",
        //     req.headers()
        // );
        match token {
            KeycloakAuthStatus::Success(t) => {
                let username = Username(t);
                let subject = Subject(t);
                let email = Email(t);
                let email_verified = EmailVerified(t);
                let display_name = DisplayName(t);
                let token_cache = GetTokenCache().await;
                let token = match token_cache.GetTokenByUsername(&username).await {
                    Err(e) => {
                        let body = Body::from(format!(
                            "auth_transform_keycloaktoken fail with error {:?} for username {}",
                            e, &username
                        ));
                        let resp = Response::builder()
                            .status(StatusCode::UNAUTHORIZED)
                            .body(body)
                            .unwrap();
                        return Ok(resp);
                        // return Err(StatusCode::UNAUTHORIZED);
                    }
                    Ok(t) => t,
                };
                if token.subject == subject
                    && token.email == email
                    && token.email_verified == email_verified
                    && token.display_name == display_name
                {
                    token
                } else {
                    let mut with_subject = (*token).clone();
                    with_subject.subject = subject;
                    with_subject.email = email;
                    with_subject.email_verified = email_verified;
                    with_subject.display_name = display_name;
                    Arc::new(with_subject)
                }
            }
            KeycloakAuthStatus::Failure(_) => match req.headers().get("Authorization") {
                None => GetTokenCache().await.anonymous.clone(),
                Some(h) => {
                    // let v = h.to_str().ok().unwrap();

                    let v = match h.to_str() {
                        Ok(val) => val,
                        Err(_) => {
                            let body = Body::from(format!("invalid auth token"));
                            let resp = Response::builder()
                                .status(StatusCode::UNAUTHORIZED)
                                .body(body)
                                .unwrap();
                            return Ok(resp);
                        }
                    };

                    // let apikey = v.strip_prefix("Bearer ").unwrap().to_owned();
                    let apikey = match v.strip_prefix("Bearer ") {
                        Some(key) => key.to_owned(),
                        None => {
                            let body = Body::from(format!("invalid auth token"));
                            let resp = Response::builder()
                                .status(StatusCode::UNAUTHORIZED)
                                .body(body)
                                .unwrap();
                            return Ok(resp);
                        }
                    };
                    match GetTokenCache().await.GetTokenByApikey(&apikey).await {
                        Err(_) => {
                            let body = Body::from(format!("invalid auth token"));
                            let resp = Response::builder()
                                .status(StatusCode::UNAUTHORIZED)
                                .body(body)
                                .unwrap();
                            return Ok(resp);
                        }
                        Ok(t) => t,
                    }
                }
            },
        }
    } else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    req.extensions_mut().insert(token);
    Ok(next.run(req).await)
}

pub struct KeycloakProvider {
    pub client: KeycloakAdmin,
}

impl fmt::Debug for KeycloakProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeycloakProvider").finish()
    }
}

impl KeycloakProvider {
    // url: "http://192.168.0.22:31260"
    // user: "admin"
    // password: "admin"
    pub async fn New(url: &str, user: &str, password: &str) -> Result<Self> {
        let client = reqwest::Client::new();
        let admin_token = KeycloakAdminToken::acquire(url, user, password, &client).await?;
        let admin = KeycloakAdmin::new(&url, admin_token, client);

        return Ok(Self { client: admin });
    }

    pub async fn GetGroups(&self, username: &str) -> Result<Vec<String>> {
        let realm = &GATEWAY_CONFIG.keycloakconfig.realm;
        let users = self
            .client
            .realm_users_get(
                realm,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(username.to_owned()),
            )
            .await?;

        if users.len() == 0 {
            return Ok(Vec::new());
        }

        if users.len() != 1 {
            error!("GetGroups multiple user has same user name {}", username);
        }

        let id = users[0].id.as_ref().unwrap().to_string();

        let user_groups = self
            .client
            .realm_users_with_id_groups_get(realm, &id, None, None, None, None)
            .await?;

        let mut groups = Vec::new();
        for group in &user_groups {
            if group.path.is_some() {
                groups.push(group.path.clone().unwrap());
            }
        }

        return Ok(groups);
    }
}
