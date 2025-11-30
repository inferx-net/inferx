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

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::data_obj::*;
use crate::resource;
use crate::resource::*;

use super::func_mgr::FuncState;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FunctionStatusDef {
    pub version: i64,

    pub state: FuncState,
    pub snapshotingFailureCnt: u64,
    pub resumingFailureCnt: u64,
}

pub type FunctionStatus = DataObject<FunctionStatusDef>;
pub type FuncStatusMgr = DataObjectMgr<FunctionStatusDef>;

impl FunctionStatus {
    pub const KEY: &'static str = "funcstatus";

    pub fn Id(&self) -> String {
        return format!(
            "{}/{}/{}/{}",
            &self.tenant, &self.namespace, &self.name, &self.object.version
        );
    }
}
