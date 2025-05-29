// Copyright (c) 2021 Quark Container Authors / 2014 The Kubernetes Authors
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

pub const FUNCPOD_TYPE: &str = "funcpod_type.qservice.io";
pub const FUNCPOD_FUNCNAME: &str = "fun_name.qservice.io";
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
pub struct HttpEndpoint {
    pub port: u16,
    #[serde(default)]
    pub schema: URIScheme,
    pub probe: String,
}

impl Default for HttpEndpoint {
    fn default() -> Self {
        return Self {
            port: 80,
            schema: URIScheme::Http,
            probe: "/health".to_owned(),
        };
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SampleCall {
    pub apiType: ApiType,
    pub path: String,
    pub prompt: String,
    #[serde(default)]
    pub imageUrl: String,
    pub body: BTreeMap<String, String>,
}

impl Default for SampleCall {
    fn default() -> Self {
        let mut map = BTreeMap::new();
        map.insert("name".to_owned(), "Unknown".to_owned());
        map.insert("max_tokens".to_owned(), "1000".to_owned());
        map.insert("temperature".to_owned(), "0".to_owned());
        map.insert("stream".to_owned(), "true".to_owned());

        return Self {
            apiType: ApiType::OpenAI,
            path: "/v1/completions".to_owned(),
            imageUrl: "".to_owned(),
            prompt: "Seattle is a".to_owned(),
            body: map,
        };
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ApiType {
    #[serde(rename = "openai")]
    OpenAI,
    #[serde(rename = "standard")]
    Standard,
    #[serde(rename = "image2text")]
    Image2Text,
    #[serde(rename = "text2img")]
    Text2Image,
}

impl Default for ApiType {
    fn default() -> Self {
        return Self::OpenAI;
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FuncSpec {
    pub image: String,
    pub commands: Vec<String>,
    pub envs: Vec<(String, String)>,
    pub mounts: Vec<Mount>,
    pub endpoint: HttpEndpoint,
    #[serde(default)]
    pub version: i64,

    #[serde(default)]
    pub entrypoint: Vec<String>,

    pub resources: Resources,

    #[serde(rename = "standby")]
    pub standby: Standby,

    #[serde(default)]
    pub probe: HttpEndpoint,

    #[serde(rename = "sample_query")]
    pub sampleCall: SampleCall,
}

fn PromptDefault() -> String {
    return "Seattle is a".to_owned();
}

impl FuncSpec {
    pub const HIBERNATE_CONTAINER_MEM_OVERHEAD: u64 = 500; // 500 * 1024 * 1024; 500 MB

    pub fn SnapshotResource(&self) -> Resources {
        return self.resources.clone();
    }

    pub fn AllResources(&self) -> Resources {
        return self.resources.clone();
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
            },
            entrypoint: Vec::new(),
            version: 0,
            resources: Resources::default(),
            standby: Standby::default(),
            probe: HttpEndpoint::default(),
            sampleCall: SampleCall::default(),
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
        let mut map = HashMap::new();

        let sample = &self.object.spec.sampleCall;
        let remainpath = sample.path.clone();

        map.insert("prompt".to_owned(), sample.prompt.clone());
        for (k, v) in &sample.body {
            map.insert(k.clone(), v.clone());
        }

        let mut jstr = String::new();
        let mut count = map.len();
        for (k, v) in map {
            count -= 1;
            if count == 0 {
                jstr += &format!("  \"{}\": \"{}\" \n", k, v);
            } else {
                jstr += &format!("  \"{}\": \"{}\", \n", k, v);
            }
        }

        let tenant = self.tenant.clone();
        let namespace = self.namespace.clone();
        let funcname = self.name.clone();

        let script = format!("curl http://localhost:4000/funccall/{tenant}/{namespace}/{funcname}/{remainpath} -H \"Content-Type: application/json\" -d '{{\n{jstr}}}'");
        return script;
    }
}

//pub type Function = Function;
