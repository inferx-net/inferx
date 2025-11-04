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
use crate::metastore::unique_id::UID;
use inferxlib::data_obj::DataObject;

use super::IxAggrStore::IxAggrStore;

#[derive(Serialize, Deserialize, Debug)]
pub struct StateSvcInfo {
    pub name: String,
    pub svcIp: String,
    pub port: u16,
}

impl StateSvcInfo {
    pub const KEY: &'static str = "statesvc";
    pub fn New(name: &str, svcIp: &str, port: u16) -> Self {
        return Self {
            name: name.to_owned(),
            svcIp: svcIp.to_owned(),
            port: port,
        };
    }

    pub fn DataObject(&self) -> DataObject<Value> {
        let s = serde_json::to_string_pretty(&self).unwrap();
        let v: Value = serde_json::from_str(&s).unwrap();
        let inner = DataObject {
            objType: Self::KEY.to_owned(),
            namespace: "system".to_owned(),
            name: self.name.to_owned(),
            object: v,
            ..Default::default()
        };

        return inner.into();
    }
}

#[derive(Debug)]
pub struct StateSvcRegister {
    pub closeNotify: Arc<Notify>,
    pub stop: AtomicBool,

    pub etcdAddresses: Vec<String>,
    pub info: StateSvcInfo,
}

impl StateSvcRegister {
    pub const LEASE_TTL: i64 = 20; // seconds

    pub async fn New(addresses: &[String], name: &str, svcIp: &str, port: u16) -> Result<Self> {
        let mut etcdAddresses = Vec::new();
        for addr in addresses {
            etcdAddresses.push(addr.clone());
        }

        let instanceName = format!("{}_{}", name, UID.get().unwrap().Get().await?);
        return Ok(Self {
            closeNotify: Arc::new(Notify::new()),
            stop: AtomicBool::new(false),

            etcdAddresses: etcdAddresses,
            info: StateSvcInfo::New(&instanceName, svcIp, port),
        });
    }

    pub async fn Process(&self, astore: &IxAggrStore) -> Result<()> {
        let store = EtcdStore::NewWithEndpoints(&self.etcdAddresses, false).await?;
        astore.WaitlistDone().await;

        let leaseId = store.LeaseGrant(Self::LEASE_TTL).await?;

        loop {
            match store.Create(&self.info.DataObject(), leaseId).await {
                Ok(_) => break,
                Err(_) => (),
            }

            store.LeaseKeepalive(leaseId).await?;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        let closeNotify = self.closeNotify.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                tokio::select! {
                    _ = closeNotify.notified() => {
                        break;
                    }
                    _ = interval.tick() => {
                        // keepalive for each 500 ms
                        store.LeaseKeepalive(leaseId).await.unwrap();
                    }
                }
            }
        });

        let _ = handle.await;

        return Ok(());
    }
}
