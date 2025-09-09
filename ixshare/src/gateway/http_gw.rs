use std::sync::Arc;

use inferxlib::{
    data_obj::DataObject,
    obj_mgr::{
        func_mgr::{FuncStatus, Function},
        funcsnapshot_mgr::FuncSnapshot,
        namespace_mgr::Namespace,
        pod_mgr::FuncPod,
        tenant_mgr::Tenant,
    },
};

use serde_json::Value;

use crate::{
    audit::{PodAuditLog, PodFailLog},
    common::*,
    metastore::selection_predicate::ListOption,
};

use super::{
    auth_layer::{AccessToken, GetTokenCache},
    gw_obj_repo::{FuncBrief, FuncDetail},
    http_gateway::HttpGateway,
};

impl HttpGateway {
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

        let version = self.client.Create(&obj).await?;

        GetTokenCache()
            .await
            .GrantTenantAdminPermission(token, &tenant.name, &token.username)
            .await?;

        return Ok(version);
    }

    pub async fn UpdateTenant(
        &self,
        _token: &Arc<AccessToken>,
        _obj: DataObject<Value>,
    ) -> Result<i64> {
        return Err(Error::CommonError(format!("doesn't support tenant update")));
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

        if !token.IsTenantAdmin(name) {
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

        GetTokenCache()
            .await
            .RevokeTenantAdminPermission(token, name, &token.username)
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

    pub async fn ListTenant(
        &self,
        token: &Arc<AccessToken>,
        _tenant: &str,
        _namespace: &str,
    ) -> Result<Vec<DataObject<Value>>> {
        let tenants = token.AdminTenants();
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
            let namespaces = token.AdminNamespaces();

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
            let namespaces = token.AdminNamespaces();
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
        let mut objs = Vec::new();
        if tenant == "" {
            let namespaces = token.AdminNamespaces();

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
            let namespaces = token.AdminNamespaces();
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
            let namespaces = token.UserNamespaces();

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
            let namespaces = token.UserNamespaces();
            for (currentTenant, namespace) in namespaces {
                if &currentTenant != tenant {
                    break;
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

    pub fn ListFuncBrief(
        &self,
        token: &Arc<AccessToken>,
        tenant: &str,
        namespace: &str,
    ) -> Result<Vec<FuncBrief>> {
        let mut objs = Vec::new();
        if tenant == "" {
            let namespaces = token.UserNamespaces();

            for (tenant, namespace) in namespaces {
                if &tenant != "public" {
                    let mut list = self.objRepo.ListFunc(&tenant, &namespace)?;
                    objs.append(&mut list);
                }
            }

            let mut list = self.objRepo.ListFunc("public", &namespace)?;
            objs.append(&mut list);
        } else if namespace == "" {
            let namespaces = token.UserNamespaces();
            for (currentTenant, namespace) in namespaces {
                if &currentTenant != tenant {
                    break;
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
        let mut objs = Vec::new();
        if tenant == "" {
            let namespaces = token.UserNamespaces();

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
            let namespaces = token.UserNamespaces();
            for (currentTenant, namespace) in namespaces {
                if &currentTenant != tenant {
                    break;
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
        let mut objs = Vec::new();
        if tenant == "" {
            let namespaces = token.UserNamespaces();

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
            let namespaces = token.UserNamespaces();
            for (currentTenant, namespace) in namespaces {
                if &currentTenant != tenant {
                    break;
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
        if !token.IsNamespaceUser(tenant, namespace) {
            return Err(Error::NoPermission);
        }

        let s = self
            .sqlAudit
            .ReadPodAudit(&tenant, &namespace, &funcname, version, id)
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
