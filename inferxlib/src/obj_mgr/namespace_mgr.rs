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

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct NamespaceObject {
    pub spec: NamespaceSpec,
    pub status: NamespaceStatus,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct NamespaceStatus {
    pub disable: bool,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct NamespaceSpec {}

pub type Namespace = DataObject<NamespaceObject>;
pub type NamespaceMgr = DataObjectMgr<NamespaceObject>;

impl Namespace {
    pub const KEY: &'static str = "namespace";
}
