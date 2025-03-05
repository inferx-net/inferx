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

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use super::funcsnapshot_mgr::*;
use crate::common::*;
use crate::data_obj::DataObject;
use crate::data_obj::DataObjectMgr;
use crate::node::ContainerDef;
use crate::resource::GPUResourceMap;
use crate::resource::GPUType;
use crate::resource::NodeResources;
use crate::resource::Resources;
use crate::resource::Standby;

use super::func_mgr::FuncSpec;
use super::func_mgr::HttpEndpoint;

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum CreatePodType {
    Normal,
    Snapshot,
    Restore,
}

impl Default for CreatePodType {
    fn default() -> Self {
        return Self::Normal;
    }
}

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
pub struct FuncPodSpec {
    pub funcname: String,
    pub fprevision: i64,
    pub id: String,
    pub uid: String,
    pub funcspec: FuncSpec,

    pub init_containers: Vec<ContainerDef>,
    pub containers: Vec<ContainerDef>,

    pub host_network: bool,
    pub nodename: String,
    pub host_ipc: bool,
    pub host_pid: bool,
    pub share_process_namespace: bool,
    pub deletion_timestamp: Option<SystemTime>,
    pub deletion_grace_period_seconds: Option<i32>,
    pub termination_grace_period_seconds: Option<i32>,
    pub runtime_class_name: Option<String>,
    pub ipAddr: u32,
    pub create_type: CreatePodType,

    //pub status: PodStatus,
    pub host_ip: String,
    pub pod_ip: String,
    pub pod_ips: Vec<String>,

    pub reqResources: Resources,
    pub allocResources: NodeResources,

    pub readinessProbe: HttpEndpoint,

    pub standby: Standby,
    pub snapshotStandbyInfo: SnapshotStandyInfo,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum ExitInfo {
    None,
    Success(String),
    Error(String),
}

impl Default for ExitInfo {
    fn default() -> Self {
        return Self::None;
    }
}

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
pub struct FuncPodStatus {
    pub stats: Option<bollard::container::Stats>,
    pub state: PodState,
    pub exitInfo: ExitInfo,
}

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
pub struct FuncPodObject {
    pub spec: FuncPodSpec,
    pub status: FuncPodStatus,
}

pub type PodMgr = DataObjectMgr<FuncPodObject>;
pub type FuncPod = DataObject<FuncPodObject>;

impl FuncPod {
    pub const KEY: &'static str = "pod";

    pub fn FuncPodKey(
        tenant: &str,
        namespace: &str,
        funcname: &str,
        revision: i64,
        id: &str,
    ) -> String {
        return format!("{}/{}/{}/{}/{}", tenant, namespace, funcname, revision, id);
    }

    pub fn FuncObjectKey(tenant: &str, namespace: &str, funcname: &str, revision: i64) -> String {
        return format!("{}/{}/{}/{}", tenant, namespace, funcname, revision);
    }

    pub fn PodKey(&self) -> String {
        return Self::FuncPodKey(
            &self.tenant,
            &self.namespace,
            &self.object.spec.funcname,
            self.object.spec.fprevision,
            &self.object.spec.id,
        );
    }

    pub fn FuncKey(&self) -> String {
        return format!(
            "{}/{}/{}/{}",
            &self.tenant, &self.namespace, &self.object.spec.funcname, self.object.spec.fprevision
        );
    }

    pub fn ImageName(&self) -> String {
        return self.object.spec.containers[0].image.clone();
    }

    pub fn PodName(&self) -> String {
        return format!("{}_{}", &self.object.spec.funcname, &self.object.spec.id);
    }

    pub fn PodNamespace(&self) -> String {
        return format!("{}/{}", &self.tenant, &self.namespace);
    }

    pub fn ToString(&self) -> String {
        return serde_json::to_string_pretty(self).unwrap();
    }

    pub fn ResumeRestore(&mut self, resources: &NodeResources) -> Result<()> {
        return self.object.spec.allocResources.Add(resources);
    }

    pub fn MemHibernateDone(&mut self) -> Result<()> {
        return self
            .object
            .spec
            .allocResources
            .Sub(&self.object.spec.allocResources.GPUResource());
    }

    pub fn MemWakeup(&mut self, gpuResources: GPUResourceMap) -> Result<()> {
        let resources = NodeResources {
            nodename: self.object.spec.nodename.clone(),
            cpu: 0,
            memory: 0,
            gpuType: GPUType::Any(),
            gpus: gpuResources,
            maxContextCnt: 0,
        };
        return self.object.spec.allocResources.Add(&resources);
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum PodState {
    // init state
    Init,
    // scheduler start to creating a new pod
    Creating,
    // a state when startup
    Created,
    // a state when container start running but th readiness probe doesn't pass
    Loading,
    // scheduler enable the pod to serve new requests
    Ready,
    // a state preserved for use to draining requests
    Draining,
    // a state qlet is killing the pod
    Terminating,
    // a normal pod exit status when nodemgr request to terminate
    Terminated,
    // a abnormal pod exit status when pod met unexpected condtion
    Failed,
    // pod artifacts are cleaned, eg. pod dir, cgroup// pod artifacts are cleaned, eg. pod dir, cgroup
    Cleanup,
    //
    Deleted,
    //
    Snapshoted,
    //
    Restoring,

    //
    Standby,

    //
    Resuming,
    //
    ResumeDone,

    // Start to hibernate the XPU HBM data to DRAM
    MemHibernating,
    // Finish to hibernate the XPU HBM data to DRAM
    MemHibernated,
    // Start to hibernate the Container state to Disk
    DiskHibernating,
    // finish to hibernate the Container state to Disk
    DiskHibernated,
    // transition state between MemHibernated to Running
    Waking,
    // transition state between Snapshot to Snapshoted
    Snapshoting,

    // Loading timeout, need rerun
    LoadingTimeout,
}

impl Default for PodState {
    fn default() -> Self {
        return Self::Init;
    }
}
