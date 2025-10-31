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
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Mutex;
use std::{collections::BTreeMap, sync::Arc, time::SystemTime};

use crate::obj_mgr::nodestatus_mgr::Quantity;
use crate::obj_mgr::pod_mgr::{FuncPod, PodState};

use super::resource::*;
use crate::common::*;

// CPU, in cores. (500m = .5 cores)
pub const ResourceCPU: &str = "cpu";
// Memory, in bytes. (500Gi = 500GiB = 500 * 1024 * 1024 * 1024)
pub const ResourceMemory: &str = "memory";
// Volume size, in bytes (e,g. 5Gi = 5GiB = 5 * 1024 * 1024 * 1024)
pub const ResourceStorage: &str = "storage";
// Local ephemeral storage, in bytes. (500Gi = 500GiB = 500 * 1024 * 1024 * 1024)
// The resource name for ResourceEphemeralStorage is alpha and it can change across releases.
pub const ResourceEphemeralStorage: &str = "ephemeral-storage";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContainerImage {
    /// Names by which this image is known. e.g. \["kubernetes.example/hyperkube:v1.0.7", "cloud-vendor.registry.example/cloud-vendor/hyperkube:v1.0.7"\]
    pub names: Vec<String>,

    /// The size of the image in bytes.
    pub size_bytes: i64,
}

pub type ReturnId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerPodState {
    Init,
    Standby,
    Resuming,
    Idle,         // no one leasing the worker
    Working(i64), // leased by one gateway, GatewayId
    Terminating,
}

impl WorkerPodState {
    pub fn IsIdle(&self) -> bool {
        match self {
            Self::Idle => return true,
            _ => return false,
        }
    }

    pub fn IsResumed(&self) -> bool {
        match self {
            Self::Idle => return true,
            Self::Working(_) => return true,
            Self::Resuming => return true,
            _ => return false,
        }
    }

    pub fn IsReady(&self) -> bool {
        match self {
            Self::Init | Self::Standby => return false,
            _ => return true,
        }
    }
}

/// VolumeMount describes a mounting of a Volume within a container.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VolumeMount {
    /// Path within the container at which the volume should be mounted.  Must not contain ':'.
    pub mount_path: String,

    /// mountPropagation determines how mounts are propagated from the host to container and the other way around. When not set, MountPropagationNone is used. This field is beta in 1.10.
    pub mount_propagation: Option<String>,

    /// This must match the Name of a Volume.
    pub name: String,

    /// Mounted read-only if true, read-write otherwise (false or unspecified). Defaults to false.
    pub read_only: Option<bool>,

    /// Path within the volume from which the container's volume should be mounted. Defaults to "" (volume's root).
    pub sub_path: Option<String>,

    /// Expanded path within the volume from which the container's volume should be mounted. Behaves similarly to SubPath but environment variable references $(VAR_NAME) are expanded using the container's environment. Defaults to "" (volume's root). SubPathExpr and SubPath are mutually exclusive.
    pub sub_path_expr: Option<String>,
}

/// ResourceRequirements describes the compute resource requirements.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceRequirements {
    /// Limits describes the maximum amount of compute resources allowed. More info: https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/
    pub limits: BTreeMap<String, Quantity>,

    /// Requests describes the minimum amount of compute resources required. If Requests is omitted for a container, it defaults to Limits if that is explicitly specified, otherwise to an implementation-defined value. More info: https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/
    pub requests: BTreeMap<String, Quantity>,
}

/// ContainerPort represents a network port in a single container.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContainerPort {
    /// Number of port to expose on the pod's IP address. This must be a valid port number, 0 \< x \< 65536.
    pub container_port: i32,

    /// What host IP to bind the external port to.
    pub host_ip: Option<String>,

    /// Number of port to expose on the host. If specified, this must be a valid port number, 0 \< x \< 65536. If HostNetwork is specified, this must match ContainerPort. Most containers do not need this.
    pub host_port: Option<i32>,

    /// If specified, this must be an IANA_SVC_NAME and unique within the pod. Each named port in a pod must have a unique name. Name for the port that can be referred to by services.
    pub name: Option<String>,

    /// Protocol for port. Must be UDP, TCP, or SCTP. Defaults to "TCP".
    ///
    pub protocol: Option<String>,
}

#[derive(Default, Serialize, Deserialize, Debug, Clone)]
pub struct ContainerDef {
    pub name: String,
    pub image: String,
    pub envs: BTreeMap<String, String>,
    pub entrypoint: Vec<String>,
    pub commands: Vec<String>,
    pub args: Vec<String>,
    pub working_dir: String,
    pub volume_mounts: Vec<VolumeMount>,
    pub stdin: bool,
    pub stdin_once: bool,
    pub resources: NodeResources,
    pub ports: Vec<ContainerPort>,
}
