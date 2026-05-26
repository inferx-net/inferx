// Copyright (c) 2025 InferX Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use inferxlib::obj_mgr::funcpolicy_mgr::{FuncPolicy, FuncPolicySpec};
use inferxlib::obj_mgr::funcstatus_mgr::{FunctionStatus, FunctionStatusDef};
use inferxlib::resource::StandbyType;
use serde_json::Value;

use super::state_svc::*;
use crate::common::*;
use crate::metastore::cache_store::BackendStore;
use inferxlib::data_obj::{DataObject, ObjRef};
use inferxlib::obj_mgr::func_mgr::{FuncState, Function};
use inferxlib::obj_mgr::namespace_mgr::Namespace;
use inferxlib::obj_mgr::tenant_mgr::{Tenant, SYSTEM_NAMESPACE, SYSTEM_TENANT};

const VIRTUAL_ENDPOINTS_NAMESPACE: &str = "endpoints";
const PLATFORM_TENANT: &str = "inferx";
const PLATFORM_SHARED_NAMESPACE: &str = "endpoint";

impl StateSvc {
    pub fn CreateObjCheck(&self, obj: &DataObject<Value>) -> Result<()> {
        match obj.objType.as_str() {
            Tenant::KEY => {
                return self.CreateTenantCheck(obj);
            }
            Namespace::KEY => {
                return self.CreateNamespaceCheck(obj);
            }
            Function::KEY => {
                return self.CreateFuncCheck(obj);
            }
            FuncPolicy::KEY => {
                return self.CreateFuncPolicyCheck(obj);
            }
            _ => (),
        }

        return Ok(());
    }

    pub async fn CreateFuncStatus(&self, dataobj: &DataObject<Value>) -> Result<()> {
        match dataobj.objType.as_str() {
            Function::KEY => {
                let func: Function = Function::FromDataObject(dataobj.clone())?;
                let status = FunctionStatusDef {
                    version: func.Version(),
                    published: Self::DefaultFunctionPublished(&func),
                    state: FuncState::Normal,
                    snapshotingFailureCnt: 0,
                    resumingFailureCnt: 0,
                };

                let funcstatus = FunctionStatus {
                    objType: FunctionStatus::KEY.to_string(),
                    tenant: func.tenant.clone(),
                    namespace: func.namespace.clone(),
                    name: func.name.clone(),
                    object: status,
                    ..Default::default()
                };

                error!("CreateFuncStatus {:#?}", &funcstatus);

                let statusDataObj = funcstatus.DataObject();

                self.store.Create(&statusDataObj, 0).await?;
            }
            _ => (),
        }

        return Ok(());
    }

    pub async fn UpdateFuncStatus(&self, dataobj: &DataObject<Value>) -> Result<()> {
        match dataobj.objType.as_str() {
            Function::KEY => {
                let func: Function = Function::FromDataObject(dataobj.clone())?;
                let status = FunctionStatusDef {
                    version: func.Version(),
                    published: Self::DefaultFunctionPublished(&func),
                    state: FuncState::Normal,
                    snapshotingFailureCnt: 0,
                    resumingFailureCnt: 0,
                };

                let funcstatus = FunctionStatus {
                    objType: FunctionStatus::KEY.to_string(),
                    tenant: func.tenant.clone(),
                    namespace: func.namespace.clone(),
                    name: func.name.clone(),
                    object: status,
                    ..Default::default()
                };

                info!("UpdateFuncStatus {:#?}", &funcstatus);

                let statusDataObj = funcstatus.DataObject();

                let key = format!(
                    "{}/{}/{}/{}",
                    FunctionStatus::KEY,
                    &func.tenant,
                    &func.namespace,
                    &func.name
                );
                let mut attempts = 0;
                loop {
                    attempts += 1;
                    let expected_rev = match self.store.Get(&key, 0).await? {
                        Some(current) => current.revision,
                        None => 0,
                    };

                    match self.store.Update(expected_rev, &statusDataObj, 0).await {
                        Ok(_) => break,
                        Err(Error::UpdateRevNotMatchErr(e)) if attempts < 3 => {
                            error!(
                                "UpdateFuncStatus conflict for {} on attempt {} (expected_rev={}, actual_rev={}), retrying",
                                key, attempts, e.expectRv, e.actualRv
                            );
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            _ => (),
        }

        return Ok(());
    }

    pub async fn DeleteFuncStatus(
        &self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        name: &str,
    ) -> Result<()> {
        match objType {
            Function::KEY => {
                let key = format!("{}/{}/{}/{}", FunctionStatus::KEY, tenant, namespace, name);

                self.store.Delete(&key, 0).await?;
            }
            _ => (),
        }

        return Ok(());
    }

    pub fn ContainersNamespace(&self, tenant: &str, namespace: &str) -> Result<()> {
        if !self
            .tenantMgr
            .Contains(SYSTEM_TENANT, SYSTEM_NAMESPACE, tenant)
        {
            return Err(Error::NotExist(format!(
                "StateSvc has no tenant {}",
                tenant
            )));
        }

        if !self
            .namespaceMgr
            .Contains(tenant, SYSTEM_NAMESPACE, namespace)
        {
            return Err(Error::NotExist(format!(
                "StateSvc has no namespace {}/{}",
                tenant, namespace
            )));
        }

        return Ok(());
    }

    pub fn CreateTenantCheck(&self, obj: &DataObject<Value>) -> Result<()> {
        let tenant = Tenant::FromDataObject(obj.clone())?;

        if &tenant.tenant != SYSTEM_TENANT || &tenant.namespace != SYSTEM_NAMESPACE {
            return Err(Error::NotExist(format!(
                "tenant must be created in tenant {}  and namespace {}",
                SYSTEM_TENANT, SYSTEM_NAMESPACE
            )));
        }

        if self
            .tenantMgr
            .Contains(SYSTEM_TENANT, SYSTEM_NAMESPACE, &tenant.name)
        {
            return Err(Error::Exist(format!(
                "StateSvc has tenant {}",
                &tenant.name
            )));
        }

        return Ok(());
    }

    pub fn CreateNamespaceCheck(&self, obj: &DataObject<Value>) -> Result<()> {
        let namespace = Namespace::FromDataObject(obj.clone())?;

        self.RejectVirtualEndpointsNamespace(&namespace.tenant, &namespace.name)?;

        if !self
            .tenantMgr
            .Contains(SYSTEM_TENANT, SYSTEM_NAMESPACE, &namespace.tenant)
        {
            return Err(Error::NotExist(format!(
                "StateSvc has no tenant {}",
                &namespace.tenant
            )));
        }

        if &namespace.namespace != SYSTEM_NAMESPACE {
            return Err(Error::NotExist(format!(
                "namespace must be created in tenant {}  and namespace {}",
                &namespace.tenant, SYSTEM_NAMESPACE
            )));
        }

        if self
            .namespaceMgr
            .Contains(&obj.tenant, &obj.namespace, &obj.name)
        {
            return Err(Error::Exist(format!(
                "StateSvc exists namespace {}",
                &obj.Key()
            )));
        }

        return Ok(());
    }

    pub fn CreateFuncCheck(&self, obj: &DataObject<Value>) -> Result<()> {
        let func = Function::FromDataObject(obj.clone())?;
        self.RejectVirtualEndpointsNamespace(&func.tenant, &func.namespace)?;
        self.ContainersNamespace(&func.tenant, &func.namespace)?;

        let tenant = self.tenantMgr.Get("system", "system", &func.tenant)?;
        let limit = &tenant.object.spec.resourceLimit;

        let funccnt = self.funcMgr.GetObjectsByPrefix(&func.tenant, "", "")?.len();

        if self
            .funcMgr
            .Contains(&obj.tenant, &obj.namespace, &obj.name)
        {
            return Err(Error::Exist(format!("StateSvc exists func {}", &obj.Key())));
        }

        error!(
            "CreateFuncCheck funccnt {} tenant {} {:#?}",
            funccnt, &func.tenant, &tenant
        );
        if funccnt >= limit.maxFuncCnt as usize {
            return Err(Error::NotExist(format!(
                "Func count {} exceed tenant limit {}",
                &funccnt, limit.maxFuncCnt
            )));
        }

        if !limit.allocMemStandby {
            let standby = &func.object.spec.standby;
            if standby.gpuMem == StandbyType::Mem
                || standby.pageableMem == StandbyType::Mem
                || standby.pinndMem == StandbyType::Mem
            {
                return Err(Error::NotExist(format!(
                    "Standby {:?} is not allowed in tenant allocMemStandby==false",
                    standby
                )));
            }
        }

        match &func.object.spec.policy {
            ObjRef::Link(_) => (),
            ObjRef::Obj(p) => self.FuncPolicyCheck(&func.tenant, p)?,
        }

        return Ok(());
    }

    pub fn CreateFuncPolicyCheck(&self, obj: &DataObject<Value>) -> Result<()> {
        let p = FuncPolicy::FromDataObject(obj.clone())?;

        Self::ValidateEndpointsFuncPolicy(&p.namespace, &p.object)?;
        self.FuncPolicyCheck(&p.tenant, &p.object)?;
        return Ok(());
    }

    pub fn FuncPolicyCheck(&self, tenant: &str, p: &FuncPolicySpec) -> Result<()> {
        let tenant = self.tenantMgr.Get("system", "system", tenant)?;
        let limit = &tenant.object.spec.resourceLimit;

        Self::ValidateTenantFuncPolicyLimit(limit, p)?;
        return Ok(());
    }

    fn ValidateTenantFuncPolicyLimit(
        limit: &inferxlib::obj_mgr::tenant_mgr::ResourceLimit,
        p: &FuncPolicySpec,
    ) -> Result<()> {
        if p.minReplica > limit.minReplicaCap {
            return Err(Error::CommonError(format!(
                "policy minReplica {} exceed limit {}",
                p.minReplica, limit.minReplicaCap
            )));
        }

        if p.maxReplica > limit.maxReplica {
            return Err(Error::CommonError(format!(
                "policy maxReplica {} exceed limit {}",
                p.maxReplica, limit.maxReplica
            )));
        }

        if p.standbyPerNode > limit.maxStandby {
            return Err(Error::CommonError(format!(
                "policy standbyPerNode {} exceed limit {}",
                p.standbyPerNode, limit.maxStandby
            )));
        }

        if p.queueLen > limit.maxQueueLen {
            return Err(Error::CommonError(format!(
                "policy QueueLen {} exceed limit {}",
                p.queueLen, limit.maxQueueLen
            )));
        }

        return Ok(());
    }

    pub fn UpdateObjCheck(&self, obj: &DataObject<Value>) -> Result<()> {
        match obj.objType.as_str() {
            Namespace::KEY => {
                let namespace = Namespace::FromDataObject(obj.clone())?;
                self.RejectVirtualEndpointsNamespace(&namespace.tenant, &namespace.name)?;
                return Ok(());
            }
            Tenant::KEY => {
                return self.UpdateTenantCheck(obj);
            }
            FunctionStatus::KEY => return Ok(()),
            Function::KEY => {
                return self.UpdateFuncCheck(obj);
            }
            FuncPolicy::KEY => {
                return self.CreateFuncPolicyCheck(obj);
            }
            _ => {
                return Err(Error::CommonError(format!(
                    "{} is not allowed update",
                    &obj.objType
                )));
            }
        }
    }

    pub fn UpdateTenantCheck(&self, obj: &DataObject<Value>) -> Result<()> {
        let tenant = Tenant::FromDataObject(obj.clone())?;

        if &tenant.tenant != SYSTEM_TENANT || &tenant.namespace != SYSTEM_NAMESPACE {
            return Err(Error::CommonError(format!(
                "tenant must be in tenant {} and namespace {}",
                SYSTEM_TENANT, SYSTEM_NAMESPACE
            )));
        }

        if !self
            .tenantMgr
            .Contains(SYSTEM_TENANT, SYSTEM_NAMESPACE, &tenant.name)
        {
            return Err(Error::NotExist(format!(
                "StateSvc doesn't exist tenant {}",
                &tenant.name
            )));
        }

        return Ok(());
    }

    pub fn UpdateFuncCheck(&self, obj: &DataObject<Value>) -> Result<()> {
        let func = Function::FromDataObject(obj.clone())?;
        self.RejectVirtualEndpointsNamespace(&func.tenant, &func.namespace)?;
        self.ContainersNamespace(&func.tenant, &func.namespace)?;

        if !self
            .funcMgr
            .Contains(&obj.tenant, &obj.namespace, &obj.name)
        {
            return Err(Error::NotExist(format!(
                "StateSvc doesn't exist func {}",
                &obj.Key()
            )));
        }

        match &func.object.spec.policy {
            ObjRef::Link(_) => (),
            ObjRef::Obj(p) => self.FuncPolicyCheck(&func.tenant, p)?,
        }

        return Ok(());
    }

    pub fn DeleteObjCheck(
        &self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        name: &str,
    ) -> Result<()> {
        match objType {
            Tenant::KEY | Namespace::KEY => {
                return Err(Error::CommonError(format!(
                    "{} is not allowed delete",
                    &objType
                )));
            }
            Function::KEY => {
                return self.DeleteFuncCheck(tenant, namespace, name);
            }
            FuncPolicy::KEY => return Ok(()),
            _ => {
                return Err(Error::CommonError(format!(
                    "{} is not allowed delete",
                    &objType
                )));
            }
        }
    }

    pub fn DeleteFuncCheck(&self, tenant: &str, namespace: &str, name: &str) -> Result<()> {
        self.ContainersNamespace(tenant, namespace)?;

        if !self.funcMgr.Contains(tenant, namespace, name) {
            return Err(Error::NotExist(format!(
                "StateSvc doesn't exist func {}/{}/{}",
                tenant, namespace, name
            )));
        }

        return Ok(());
    }

    fn RejectVirtualEndpointsNamespace(&self, tenant: &str, namespace: &str) -> Result<()> {
        if tenant != SYSTEM_TENANT && namespace == VIRTUAL_ENDPOINTS_NAMESPACE {
            return Err(Error::CommonError(format!(
                "namespace {} is reserved for virtual endpoints; only FuncPolicy objects are allowed there",
                VIRTUAL_ENDPOINTS_NAMESPACE
            )));
        }

        Ok(())
    }

    fn DefaultFunctionPublished(func: &Function) -> bool {
        !Self::IsPlatformSharedFunc(&func.tenant, &func.namespace)
    }

    fn IsPlatformSharedFunc(tenant: &str, namespace: &str) -> bool {
        tenant == PLATFORM_TENANT && namespace == PLATFORM_SHARED_NAMESPACE
    }

    fn ValidateEndpointsFuncPolicy(namespace: &str, p: &FuncPolicySpec) -> Result<()> {
        if namespace != VIRTUAL_ENDPOINTS_NAMESPACE {
            return Ok(());
        }

        if p.minReplica > 0 {
            return Err(Error::CommonError(
                "min_replica is not supported for endpoints FuncPolicy; standby is controlled by the platform function"
                    .to_string(),
            ));
        }

        if p.standbyPerNode > 0 {
            return Err(Error::CommonError(
                "standby_per_node is not supported for endpoints FuncPolicy; standby is controlled by the platform function"
                    .to_string(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::StateSvc;
    use inferxlib::obj_mgr::funcpolicy_mgr::FuncPolicySpec;
    use inferxlib::obj_mgr::tenant_mgr::ResourceLimit;

    #[test]
    fn default_function_published_for_platform_shared_is_false() {
        assert!(StateSvc::IsPlatformSharedFunc("inferx", "endpoint"));
    }

    #[test]
    fn default_function_published_for_other_namespaces_is_true() {
        assert!(!StateSvc::IsPlatformSharedFunc("tenant-a", "endpoint"));
        assert!(!StateSvc::IsPlatformSharedFunc("inferx", "models"));
    }

    #[test]
    fn endpoints_policy_rejects_min_replica() {
        let policy = FuncPolicySpec {
            minReplica: 1,
            ..Default::default()
        };

        assert!(StateSvc::ValidateEndpointsFuncPolicy("endpoints", &policy).is_err());
    }

    #[test]
    fn endpoints_policy_rejects_standby_per_node() {
        let policy = FuncPolicySpec {
            standbyPerNode: 1,
            ..Default::default()
        };

        assert!(StateSvc::ValidateEndpointsFuncPolicy("endpoints", &policy).is_err());
    }

    #[test]
    fn tenant_policy_rejects_min_replica_when_cap_is_zero() {
        let policy = FuncPolicySpec {
            minReplica: 1,
            ..Default::default()
        };

        let limit = ResourceLimit {
            minReplicaCap: 0,
            ..Default::default()
        };

        assert!(StateSvc::ValidateTenantFuncPolicyLimit(&limit, &policy).is_err());
    }

    #[test]
    fn tenant_policy_accepts_min_replica_within_cap() {
        let policy = FuncPolicySpec {
            minReplica: 2,
            ..Default::default()
        };

        let limit = ResourceLimit {
            minReplicaCap: 2,
            ..Default::default()
        };

        assert!(StateSvc::ValidateTenantFuncPolicyLimit(&limit, &policy).is_ok());
    }

    #[test]
    fn tenant_policy_rejects_min_replica_above_cap() {
        let policy = FuncPolicySpec {
            minReplica: 3,
            ..Default::default()
        };

        let limit = ResourceLimit {
            minReplicaCap: 2,
            ..Default::default()
        };

        assert!(StateSvc::ValidateTenantFuncPolicyLimit(&limit, &policy).is_err());
    }
}
