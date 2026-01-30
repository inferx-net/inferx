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
use crate::{common::*, node_config::NODE_CONFIG};

pub const SECRET_ADDR: &str = "postgresql://audit_user:123456@localhost:5431/secretdb";

static TOKEN_CACHE: OnceCell<TokenCache> = OnceCell::const_new();

pub async fn GetTokenCache() -> &'static TokenCache {
    TOKEN_CACHE
        .get_or_init(|| async {
            info!(
                "GetTokenCache config {:?} secretstore addr {}",
                &NODE_CONFIG.keycloakconfig, &GATEWAY_CONFIG.secretStoreAddr
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

#[derive(Debug)]
pub struct AccessToken {
    pub username: String,
    pub roles: BTreeSet<String>,
    pub apiKeys: Vec<String>,
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
            username: username.to_owned(),
            roles: set,
            apiKeys: apikeys,
            updatetime: SystemTime::now(),
        };
    }

    pub fn InferxAdminToken() -> Self {
        return Self {
            username: Self::INERX_ADMIN.to_owned(),
            roles: BTreeSet::new(),
            apiKeys: Vec::new(),
            updatetime: SystemTime::now(),
        };
    }

    pub fn IsInferxAdmin(&self) -> bool {
        let prefix = Self::TENANT_ADMIN;
        let systemtenant = Self::SYSTEM_TENANT;
        let isSystemAdmin = self.roles.contains(&format!("{prefix}{systemtenant}"));

        return &self.username == Self::INERX_ADMIN || isSystemAdmin;
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
        if self.IsInferxAdmin() {
            return true;
        }

        if tenant == "public" {
            return true;
        }
        let prefix = Self::TENANT_USER;
        return self.roles.contains(&format!("{prefix}{tenant}")) || self.IsTenantAdmin(tenant);
    }

    pub fn IsNamespaceUser(&self, tenant: &str, namespace: &str) -> bool {
        if self.IsInferxAdmin() {
            return true;
        }

        if tenant == "public" {
            return true;
        }
        let prefix = Self::NAMSPACE_USER;
        return self
            .roles
            .contains(&format!("{prefix}{tenant}/{namespace}"))
            || self.IsNamespaceAdmin(tenant, namespace);
    }

    pub fn IsTenantAdmin(&self, tenant: &str) -> bool {
        if self.IsInferxAdmin() {
            return true;
        }

        let prefix = Self::TENANT_ADMIN;
        return self.roles.contains(&format!("{prefix}{tenant}"));
    }

    pub fn IsNamespaceAdmin(&self, tenant: &str, namespace: &str) -> bool {
        if self.IsInferxAdmin() {
            return true;
        }

        let prefix = Self::NAMSPACE_ADMIN;
        return self
            .roles
            .contains(&format!("{prefix}{tenant}/{namespace}"));
    }

    pub fn AdminTenants(&self) -> Vec<String> {
        let mut items = Vec::new();
        let prefix = Self::TENANT_ADMIN;
        for elem in self.roles.range::<str, _>((Included(prefix), Unbounded)) {
            match elem.strip_prefix(prefix) {
                None => break,
                Some(tenant) => {
                    items.push(tenant.to_owned());
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
                    items.push((splits[0].to_owned(), splits[1].to_owned()));
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
                    items.insert(tenant.to_owned());
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
        v.push("public".to_owned());

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
                    items.insert((splits[0].to_owned(), splits[1].to_owned()));
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
                username: "anonymous".to_owned(),
                roles: BTreeSet::new(),
                apiKeys: Vec::new(),
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

        for k in &newtoken.apiKeys {
            self.apikeyStore
                .lock()
                .unwrap()
                .insert(k.clone(), newtoken.clone());
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

    pub async fn GetTokenByApikey(&self, apikey: &str) -> Result<Arc<AccessToken>> {
        if GATEWAY_CONFIG.inferxAdminApikey.len() > 0 && apikey == &GATEWAY_CONFIG.inferxAdminApikey
        {
            return Ok(Arc::new(AccessToken::InferxAdminToken()));
        }

        let oldToken = match self.apikeyStore.lock().unwrap().get(apikey) {
            Some(g) => {
                if g.Timeout() {
                    Some(g.clone())
                } else {
                    return Ok(g.clone());
                }
            }
            None => None,
        };

        let user = self.sqlstore.GetApikey(apikey).await?;
        let apikeys = self.sqlstore.GetApikeys(&user.username).await?;
        let mut keys = Vec::new();
        for u in apikeys {
            keys.push(u.apikey.clone());
        }

        let groups = self.sqlstore.GetRoles(&user.username).await?;
        let newToken = Arc::new(AccessToken::New(&user.username, groups, keys));

        self.Replace(oldToken, &newToken);

        return Ok(newToken);
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
        username: &str,
        keyname: &str,
    ) -> Result<String> {
        let username = if username.len() == 0 {
            token.username.clone()
        } else if username == &token.username {
            token.username.clone()
        } else {
            if !token.IsInferxAdmin() {
                return Err(Error::NoPermission);
            }
            token.username.clone()
        };

        let apikey = uuid::Uuid::new_v4().to_string();
        self.sqlstore
            .CreateApikey(&apikey, &username, keyname)
            .await?;
        return Ok(apikey);
    }

    pub async fn DeleteApiKey(
        &self,
        token: &Arc<AccessToken>,
        keyname: &str,
        username: &str,
    ) -> Result<bool> {
        let username = if username.len() == 0 {
            token.username.clone()
        } else if username == &token.username {
            token.username.clone()
        } else {
            if !token.IsInferxAdmin() {
                return Err(Error::NoPermission);
            }
            token.username.clone()
        };

        let res = self.sqlstore.DeleteApikey(keyname, &username).await;
        return Ok(res);
    }

    pub async fn GetApikeys(&self, username: &str) -> Result<Vec<Apikey>> {
        let keys = self.sqlstore.GetApikeys(username).await?;
        return Ok(keys);
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
                let token = match GetTokenCache().await.GetTokenByUsername(&username).await {
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
                token
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
