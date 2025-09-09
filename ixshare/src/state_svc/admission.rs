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

use serde_json::Value;

use super::state_svc::*;
use crate::common::*;
use inferxlib::data_obj::DataObject;
use inferxlib::obj_mgr::func_mgr::Function;
use inferxlib::obj_mgr::namespace_mgr::Namespace;
use inferxlib::obj_mgr::tenant_mgr::{Tenant, SYSTEM_NAMESPACE, SYSTEM_TENANT};

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
        self.ContainersNamespace(&func.tenant, &func.namespace)?;

        if self
            .funcMgr
            .Contains(&obj.tenant, &obj.namespace, &obj.name)
        {
            return Err(Error::Exist(format!("StateSvc exists func {}", &obj.Key())));
        }

        return Ok(());
    }

    pub fn UpdateObjCheck(&self, obj: &DataObject<Value>) -> Result<()> {
        match obj.objType.as_str() {
            Tenant::KEY | Namespace::KEY => {
                return Err(Error::CommonError(format!(
                    "{} is not allowed update",
                    &obj.objType
                )));
            }
            Function::KEY => {
                return self.UpdateFuncCheck(obj);
            }
            _ => {
                return Err(Error::CommonError(format!(
                    "{} is not allowed update",
                    &obj.objType
                )));
            }
        }
    }

    pub fn UpdateFuncCheck(&self, obj: &DataObject<Value>) -> Result<()> {
        let func = Function::FromDataObject(obj.clone())?;
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
}
