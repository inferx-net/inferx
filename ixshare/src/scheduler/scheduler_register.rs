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

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Notify;

use crate::common::*;
use crate::etcd::etcd_store::EtcdStore;
use inferxlib::data_obj::DataObject;

#[derive(Serialize, Deserialize, Debug)]
pub struct SchedulerInfo {
    pub svcIp: String,
    pub port: u16,
}

impl SchedulerInfo {
    pub const KEY: &'static str = "scheduler";
    pub fn New(svcIp: &str, port: u16) -> Self {
        return Self {
            svcIp: svcIp.to_owned(),
            port: port,
        };
    }

    pub fn DataObject(&self) -> DataObject<Value> {
        let s = serde_json::to_string_pretty(&self).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        let inner = DataObject {
            objType: Self::KEY.to_owned(),
            tenant: "system".to_owned(),
            namespace: "system".to_owned(),
            name: Self::KEY.to_owned(),
            object: v,
            ..Default::default()
        };

        return inner.into();
    }

    pub fn FromDataObject(obj: DataObject<Value>) -> Result<Self> {
        let info = match serde_json::from_value::<Self>(obj.object.clone()) {
            Err(e) => {
                return Err(Error::CommonError(format!(
                    "SchedulerInfo::FromDataObject {:?}",
                    e
                )))
            }
            Ok(s) => s,
        };
        return Ok(info);
    }

    pub fn SchedulerUrl(&self) -> String {
        return format!("http://{}:{}", self.svcIp, self.port);
    }
}

#[derive(Debug)]
pub struct SchedulerRegister {
    pub closeNotify: Arc<Notify>,
    pub stop: AtomicBool,

    pub etcdAddresses: Vec<String>,
    pub info: SchedulerInfo,

    pub store: EtcdStore,
}

impl SchedulerRegister {
    pub const LEASE_TTL: i64 = 5; // seconds

    pub async fn New(addresses: &[String], svcIp: &str, port: u16) -> Result<Self> {
        let mut etcdAddresses = Vec::new();
        for addr in addresses {
            etcdAddresses.push(addr.clone());
        }

        let store = EtcdStore::NewWithEndpoints(&etcdAddresses, false).await?;

        return Ok(Self {
            closeNotify: Arc::new(Notify::new()),
            stop: AtomicBool::new(false),

            etcdAddresses: etcdAddresses,
            info: SchedulerInfo::New(svcIp, port),

            store: store,
        });
    }

    pub async fn CreateObject(&self) -> Result<i64> {
        let leaseId = self.store.LeaseGrant(Self::LEASE_TTL).await?;

        loop {
            match self.store.Create(&self.info.DataObject(), leaseId).await {
                Ok(_) => return Ok(leaseId),
                Err(_) => (),
            }

            self.store.LeaseKeepalive(leaseId).await?;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    pub async fn Process(&self, leaseId: i64) -> Result<()> {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));

        loop {
            tokio::select! {
                _ = self.closeNotify.notified() => {
                    return Ok(());
                }
                _ = interval.tick() => {
                    // keepalive for each 500 ms
                    self.store.LeaseKeepalive(leaseId).await?;
                }
            }
        }
    }
}
