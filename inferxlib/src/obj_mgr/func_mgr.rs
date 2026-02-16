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

use std::cmp::max;
use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::data_obj::*;
use crate::resource;
use crate::resource::*;

use super::funcpolicy_mgr::FuncPolicy;
use super::funcpolicy_mgr::FuncPolicySpec;

pub const FUNCPOD_TYPE: &str = "funcpod_type.inferx.io";
pub const FUNCPOD_FUNCNAME: &str = "fun_name.inferx.io";
pub const FUNCPOD_PROMPT: &str = "prompt";

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct FuncPackageId {
    pub namespace: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]

pub struct Mount {
    pub hostpath: String,
    pub mountpath: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum URIScheme {
    Http,
    Https,
}

impl Default for URIScheme {
    fn default() -> Self {
        return Self::Http;
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ProbeType {
    HealthCheck,
    Prompt,
}

fn ProbeTypeDefault() -> ProbeType {
    return ProbeType::Prompt;
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HttpEndpoint {
    pub port: u16,
    #[serde(default)]
    pub schema: URIScheme,
    pub probe: String,
    #[serde(default = "ProbeTimeoutDefault")]
    pub probeTimeout: u32,
    #[serde(default = "ProbeTypeDefault")]
    pub probetype: ProbeType,
}

fn ProbeTimeoutDefault() -> u32 {
    return 1000;
}

impl Default for HttpEndpoint {
    fn default() -> Self {
        return Self {
            port: 80,
            schema: URIScheme::Http,
            probe: "/health".to_owned(),
            probeTimeout: 1000,
            probetype: ProbeType::Prompt,
        };
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SampleCall {
    pub apiType: ApiType,
    pub path: String,
    pub prompt: String,
    #[serde(default)]
    pub prompts: Vec<String>,
    #[serde(default)]
    pub imageUrl: String,
    pub body: serde_json::Value,

    #[serde(default = "DefaultLoadingTimeout")]
    pub loadingTimeout: u64, // loading timeout, minutes
}

impl Default for SampleCall {
    fn default() -> Self {
        return Self {
            apiType: ApiType::Text2Text,
            path: "/v1/completions".to_owned(),
            imageUrl: "".to_owned(),
            prompt: "Seattle is a".to_owned(),
            prompts: Vec::new(),
            body: json!({
                "name": "Unknown",
                "max_tokens": 1000,
                "temperature": 0,
                "stream": true
            }),
            loadingTimeout: 90,
        };
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ScheduleConfig {
    #[serde(rename = "min_replica")]
    pub minReplica: u64,
    #[serde(rename = "max_replica")]
    pub maxReplica: u64,
    #[serde(rename = "standby_per_node")]
    pub standbyPerNode: u64,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        return Self {
            minReplica: 0,
            maxReplica: 10,
            standbyPerNode: 1,
        };
    }
}

pub fn DefaultScheduleConfig() -> ScheduleConfig {
    return ScheduleConfig::default();
}

pub fn DefaultLoadingTimeout() -> u64 {
    return 90;
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ApiType {
    #[serde(rename = "text2text")]
    Text2Text,
    #[serde(rename = "image2text")]
    Image2Text,
    #[serde(rename = "text2img")]
    Text2Image,
    #[serde(rename = "text2audio")]
    Text2Audio,
}

impl Default for ApiType {
    fn default() -> Self {
        return Self::Text2Text;
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ModelType {
    Public,
    Private,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FuncSpec {
    pub image: String,
    pub commands: Vec<String>,
    pub envs: Vec<(String, String)>,
    #[serde(default)]
    pub mounts: Vec<Mount>,
    pub endpoint: HttpEndpoint,
    #[serde(default)]
    pub version: i64,

    #[serde(rename = "model_type", default = "ModelTypeDefault")]
    pub modelType: ModelType,

    #[serde(default)]
    pub entrypoint: Vec<String>,

    pub resources: Resources,

    #[serde(rename = "standby")]
    pub standby: Standby,

    #[serde(rename = "sample_query")]
    pub sampleCall: SampleCall,

    #[serde(default = "FuncpolicyDefault")]
    pub policy: ObjRef<FuncPolicySpec>,
}

fn PromptDefault() -> String {
    return "Seattle is a".to_owned();
}

fn FuncpolicyDefault() -> ObjRef<FuncPolicySpec> {
    return ObjRef::Obj(FuncPolicySpec::default());
}

fn ModelTypeDefault() -> ModelType {
    return ModelType::Public;
}

impl FuncSpec {
    pub const HIBERNATE_CONTAINER_MEM_OVERHEAD: u64 = 500; // 500 * 1024 * 1024; 500 MB

    pub fn SnapshotResource(&self, contextCnt: u64) -> Resources {
        let gpuStandy = self.standby.gpuMem;
        let mut resource = self.resources.clone();
        if gpuStandy == StandbyType::Mem {
            // when in standby state, the nodeagent has to cache the gpu data in cpu memory after snapshot is done.
            // so we need extra cpu memory to cache gpu data
            resource.memory = max(resource.memory, resource.gpu.vRam * resource.gpu.gpuCount);
        }
        resource.gpu.contextCount = contextCnt;
        return resource;
    }

    pub fn RunningResource(&self) -> Resources {
        let mut resource = self.resources.clone();
        resource.gpu.contextCount = 1;
        return resource;
    }
}

fn port_default() -> u16 {
    return 80;
}

impl Default for FuncSpec {
    fn default() -> Self {
        return Self {
            image: String::new(),
            commands: Vec::new(),
            envs: Vec::new(),
            mounts: Vec::new(),
            endpoint: HttpEndpoint {
                port: 80,
                probe: "/health".to_owned(),
                schema: URIScheme::Http,
                probeTimeout: 1000,
                probetype: ProbeType::Prompt,
            },
            modelType: ModelType::Public,
            entrypoint: Vec::new(),
            version: 0,
            resources: Resources::default(),
            standby: Standby::default(),
            sampleCall: SampleCall::default(),
            policy: Default::default(),
        };
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Copy)]
pub enum FuncState {
    Normal,
    Fail,
}

impl Default for FuncState {
    fn default() -> Self {
        return Self::Normal;
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FuncStatus {
    pub state: FuncState,
    pub snapshotingFailureCnt: u64,
    pub resumingFailureCnt: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FuncObject {
    pub spec: FuncSpec,

    #[serde(default)]
    pub status: FuncStatus,
}

pub type Function = DataObject<FuncObject>;
pub type FuncMgr = DataObjectMgr<FuncObject>;

impl Function {
    pub const KEY: &'static str = "function";

    pub fn Id(&self) -> String {
        return format!(
            "{}/{}/{}/{}",
            &self.tenant,
            &self.namespace,
            &self.name,
            &self.Version()
        );
    }

    pub fn Version(&self) -> i64 {
        return self.object.spec.version;
    }

    pub fn SampleRestCall(&self) -> String {
        let sample = &self.object.spec.sampleCall;
        let remainpath = sample.path.clone();

        // 1. Create a base JSON object with the prompt
        // 2. Merge the 'body' Value into it
        let mut full_body = json!({
            "prompt": sample.prompt
        });

        // If 'body' is an object, merge its keys into our full_body
        if let Some(body_obj) = sample.body.as_object() {
            if let Some(full_obj) = full_body.as_object_mut() {
                for (k, v) in body_obj {
                    full_obj.insert(k.clone(), v.clone());
                }
            }
        }

        // 3. Serialize to a pretty-printed or compact string
        let jstr = serde_json::to_string_pretty(&full_body).unwrap_or_default();

        let tenant = self.tenant.clone();
        let namespace = self.namespace.clone();
        let funcname = self.name.clone();

        // 4. Return the formatted curl command
        // Note: We use the serialized JSON string directly in the -d flag
        format!(
            "curl http://localhost:4000/funccall/{tenant}/{namespace}/{funcname}/{remainpath} \
            -H \"Content-Type: application/json\" \
            -d '{jstr}'"
        )
    }
}

//pub type Function = Function;
