use std::{collections::BTreeSet, sync::Arc};

use inferxlib::{
    data_obj::DataObject,
    obj_mgr::{
        func_mgr::{FuncStatus, Function},
        funcpolicy_mgr::FuncPolicy,
        funcsnapshot_mgr::FuncSnapshot,
        namespace_mgr::Namespace,
        pod_mgr::FuncPod,
        tenant_mgr::Tenant,
    },
};

use serde_json::Value;

use crate::{
    audit::{PodAuditLog, PodFailLog, SnapshotScheduleAudit},
    common::*,
    metastore::selection_predicate::ListOption,
};

use super::{
    auth_layer::{AccessToken, GetTokenCache, ObjectType, PermissionType, UserRole},
    gw_obj_repo::{FuncBrief, FuncDetail},
    http_gateway::HttpGateway,
};

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

    pub async fn CreateTenant(
        &self,
        token: &Arc<AccessToken>,
        obj: DataObject<Value>,
    ) -> Result<i64> {
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
        if !token.IsInferxAdmin() {
            return Err(Error::NoPermission);
        }

        // let mut dataobj = obj;
        // let tenant = Tenant::FromDataObject(dataobj)?;
        // dataobj = tenant.DataObject();
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
                    error!("AdminNamespaces fail to list tenant {} namespaces: {:?}", tenant, e);
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

        let mut namespaces = BTreeSet::new();
        for (tenant, namespace) in token.UserNamespaces() {
            namespaces.insert((tenant, namespace));
        }

        // Tenant users/admins are namespace users for every namespace in their tenant.
        for tenant in token.UserTenants() {
            match self.objRepo.namespaceMgr.GetObjects(&tenant, "") {
                Ok(items) => {
                    for ns in items {
                        namespaces.insert((ns.Tenant(), ns.Name()));
                    }
                }
                Err(e) => {
                    error!("UserNamespaces fail to list tenant {} namespaces: {:?}", tenant, e);
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
