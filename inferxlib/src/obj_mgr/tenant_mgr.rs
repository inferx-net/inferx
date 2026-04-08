// Copyright (c) 2025 InferX Authors / 2014 The Kubernetes Authors
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

use crate::data_obj::*;

pub const SYSTEM_TENANT: &str = "system";
pub const SYSTEM_NAMESPACE: &str = "system";

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct TenantObject {
    pub spec: TenantSpec,
    pub status: TenantStatus,
}

fn default_resourcelimit() -> ResourceLimit {
    return ResourceLimit {
        maxFuncCnt: 6,
        allocMemStandby: false,
        maxReplica: 2,
        maxStandby: 1,
        maxQueueLen: 100,
    };
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct TenantStatus {
    pub disable: bool,
    #[serde(default)]
    pub quota_exceeded: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ResourceLimit {
    #[serde(default = "default_funccnt", rename = "max_funccount")]
    pub maxFuncCnt: u64,
    #[serde(default = "default_allow_mem_standby", rename = "allow_mem_standby")]
    pub allocMemStandby: bool,
    #[serde(default = "default_max_replica", rename = "max_replica")]
    pub maxReplica: u64,
    #[serde(default = "default_max_standby", rename = "max_standby")]
    pub maxStandby: u64,
    #[serde(default = "default_max_queue_len", rename = "max_queue_len")]
    pub maxQueueLen: usize,
}

impl Default for ResourceLimit {
    fn default() -> Self {
        default_resourcelimit()
    }
}

fn default_funccnt() -> u64 {
    10
}

fn default_allow_mem_standby() -> bool {
    false
}

fn default_max_replica() -> u64 {
    2
}

fn default_max_standby() -> u64 {
    1
}

fn default_max_queue_len() -> usize {
    100
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TenantSpec {
    #[serde(default = "default_resourcelimit", rename = "limit")]
    pub resourceLimit: ResourceLimit,
}

impl Default for TenantSpec {
    fn default() -> Self {
        Self {
            resourceLimit: default_resourcelimit(),
        }
    }
}

pub type Tenant = DataObject<TenantObject>;

impl Tenant {
    pub const KEY: &'static str = "tenant";
}

pub type TenantMgr = DataObjectMgr<TenantObject>;
