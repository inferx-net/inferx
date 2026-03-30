use std::{collections::BTreeSet, sync::Arc};

use inferxlib::{
    data_obj::DataObject,
    obj_mgr::{
        func_mgr::{FuncStatus, Function},
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
};

use super::{
    auth_layer::{
        AccessToken, ApikeyCreateRequest, GetTokenCache, ObjectType, PermissionType, UserRole,
    },
    gw_obj_repo::{FuncBrief, FuncDetail},
    http_gateway::{HttpGateway, GATEWAY_CONFIG},
};

const ONBOARD_TENANT_PREFIX: &str = "tn-";
const ONBOARD_TENANT_SUFFIX_LEN: usize = 10;
const ONBOARD_DEFAULT_NAMESPACE: &str = "default";
const ONBOARD_MAX_ATTEMPTS: usize = 3;
const ONBOARD_APIKEY_PREFIX: &str = "quickstart-inference";
const ONBOARD_INITIAL_CREDIT_NOTE: &str = "Initial onboarding credit";
const ONBOARD_INITIAL_CREDIT_ADDED_BY: &str = "system-onboard";
const ONBOARD_INITIAL_CREDIT_PAYMENT_REF_PREFIX: &str = "onboard-initial-credit";

impl HttpGateway {
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

        return self.client.Create(&dataobj).await;
    }

    pub async fn CreateFuncPolicy(
        &self,
        token: &Arc<AccessToken>,
        obj: DataObject<Value>,
    ) -> Result<i64> {
        let dataobj = obj;

        let funcpolicy = FuncPolicy::FromDataObject(dataobj.clone())?;

        let tenant = funcpolicy.tenant.clone();
        let namespace = funcpolicy.namespace.clone();

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
        let id = self.client.Uid().await?;
        func.object.spec.version = id;
        func.object.status = FuncStatus::default();
        dataobj = func.DataObject();
        let version = self.client.Update(&dataobj, 0).await?;
        return Ok(version);
    }

    pub async fn UpdateFuncPolicy(
        &self,
        token: &Arc<AccessToken>,
        obj: DataObject<Value>,
    ) -> Result<i64> {
        let dataobj = obj;

        let funcpolicy = FuncPolicy::FromDataObject(dataobj.clone())?;

        let tenant = funcpolicy.tenant.clone();
        let namespace = funcpolicy.namespace.clone();

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
