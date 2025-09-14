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

use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::etcd::etcd_store::EtcdStore;

use crate::common::*;

use inferxlib::data_obj::*;

pub static UID: OnceCell<UniqueId> = OnceCell::new();

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Uid {}

impl Uid {
    pub const KEY: &'static str = "unique_id";
    pub fn New() -> Self {
        return Self {};
    }

    pub fn DataObject() -> DataObject<Value> {
        let s = serde_json::to_string_pretty(&Uid {}).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        let inner = DataObject {
            objType: Self::KEY.to_owned(),
            namespace: "system".to_owned(),
            name: Self::KEY.to_owned(),
            object: v,
            ..Default::default()
        };

        return inner.into();
    }
}

#[derive(Debug)]
pub struct UniqueId {
    pub store: EtcdStore,
}

impl UniqueId {
    pub async fn New(addresses: &[String]) -> Result<Self> {
        let store = EtcdStore::NewWithEndpoints(addresses, false).await?;

        return Ok(Self { store: store });
    }

    pub async fn Get(&self) -> Result<i64> {
        return self.store.GetRev(&Uid::DataObject()).await;
    }
}
