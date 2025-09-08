// Copyright (c) 2023 Quark Container Authors
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

use crate::obj_mgr::pod_mgr::{FuncPod, PodState};

use super::resource::*;
use crate::common::*;


#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeSystemInfo {
    /// The Architecture reported by the node
    pub architecture: String,

    /// ContainerRuntime Version reported by the node through runtime remote API (e.g. containerd://1.4.2).
    pub container_runtime_version: String,

    /// Kernel Version reported by the node from 'uname -r' (e.g. 3.16.0-0.bpo.4-amd64).
    pub kernel_version: String,

    /// MachineID reported by the node. For unique machine identification in the cluster this field is preferred. Learn more from man(5) machine-id: http://man7.org/linux/man-pages/man5/machine-id.5.html
    pub machine_id: String,

    /// The Operating System reported by the node
    pub operating_system: String,

    /// OS Image reported by the node from /etc/os-release (e.g. Debian GNU/Linux 7 (wheezy)).
    pub os_image: String,

    pub system_uuid: String,

    pub boot_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeAddress {
    /// The node address.
    pub address: String,

    /// Node address type, one of Hostname, ExternalIP or InternalIP.
    pub type_: String,
}

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
pub struct Quantity(pub i64);

/// NodeCondition contains condition information for a node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeCondition {
    /// Last time we got an update on a given condition.
    pub last_heartbeat_time: Option<SystemTime>,

    /// Last time the condition transit from one status to another.
    pub last_transition_time: Option<SystemTime>,

    /// Human readable message indicating details about last transition.
    pub message: Option<String>,

    /// (brief) reason for the condition's last transition.
    pub reason: Option<String>,

    /// Status of the condition, one of True, False, Unknown.
    pub status: String,

    /// Type of node condition.
    pub type_: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContainerImage {
    /// Names by which this image is known. e.g. \["kubernetes.example/hyperkube:v1.0.7", "cloud-vendor.registry.example/cloud-vendor/hyperkube:v1.0.7"\]
    pub names: Vec<String>,

    /// The size of the image in bytes.
    pub size_bytes: i64,
}

/// ObjectMeta is metadata that all persisted resources must have, which includes all objects users must create.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub name: String,
    pub namespace: String,
    pub uid: String,
    pub resource_version: String,
    pub labels: BTreeMap<String, String>,
    /// Annotations is an unstructured key value map stored with a resource that may be set by external tools to store and retrieve arbitrary metadata. They are not queryable and should be preserved when modifying objects. More info: http://kubernetes.io/docs/user-guide/annotations
    pub annotations: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    /////////////// metadata //////////////////////////
    pub name: String,
    pub tenant: String,
    pub namespace: String,
    pub uid: String,
    pub resource_version: String,
    pub labels: BTreeMap<String, String>,
    /// Annotations is an unstructured key value map stored with a resource that may be set by external tools to store and retrieve arbitrary metadata. They are not queryable and should be preserved when modifying objects. More info: http://kubernetes.io/docs/user-guide/annotations
    pub annotations: BTreeMap<String, String>,

    //////////////// spec ////////////////////
    pub node_ip: String,

    /// PodCIDR represents the pod IP range assigned to the node.
    pub pod_cidr: String,

    /// Unschedulable controls node schedulability of new pods. By default, node is schedulable. More info: https://kubernetes.io/docs/concepts/nodes/node/#manual-node-administration
    pub unschedulable: bool,

    // pub nodeResources: NodeResources,

    //pub spec: NodeDef,
    pub status: NodeStatus,
    pub total: NodeResources,
    pub available: NodeResources,
}

impl Node {
    pub fn NodeId(&self) -> String {
        return format!("{}/{}/{}", &self.tenant, &self.namespace, &self.name);
    }

    pub fn ToString(&self) -> String {
        return serde_json::to_string_pretty(self).unwrap();
    }

    pub fn FromString(s: &str) -> Result<Self> {
        let p: Node = serde_json::from_str(s)?;
        return Ok(p);
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeDef {
    pub node_ip: String,

    /// PodCIDR represents the pod IP range assigned to the node.
    pub pod_cidr: String,

    /// Unschedulable controls node schedulability of new pods. By default, node is schedulable. More info: https://kubernetes.io/docs/concepts/nodes/node/#manual-node-administration
    pub unschedulable: bool,
}

/// NodeStatus is information about the current status of a node.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeStatus {
    pub phase: String,

    /// List of addresses reachable to the node
    pub addresses: Vec<NodeAddress>,

    /// Allocatable represents the resources of a node that are available for scheduling. Defaults to Capacity.
    pub allocatable: BTreeMap<String, Quantity>,

    /// Capacity represents the total resources of a node
    pub capacity: std::collections::BTreeMap<String, Quantity>,

    /// Conditions is an array of current observed node conditions.
    pub conditions: Vec<NodeCondition>,

    // pub config: QletConfig,
    /// List of container images on this node
    pub images: Vec<ContainerImage>,

    /// Set of ids/uuids to uniquely identify the node
    pub node_info: NodeSystemInfo,
    // /// List of volumes that are attached to the node.
    // pub volumes_attached: Option<Vec<crate::api::core::v1::AttachedVolume>>,

    // /// List of attachable volumes in use (mounted) by the node.
    // pub volumes_in_use: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct NodeInfo {
    /// The Architecture reported by the node
    pub architecture: String,

    /// Boot ID reported by the node.
    pub boot_id: String,

    /// ContainerRuntime Version reported by the node through runtime remote API (e.g. containerd://1.4.2).
    pub container_runtime_version: String,

    /// Kernel Version reported by the node from 'uname -r' (e.g. 3.16.0-0.bpo.4-amd64).
    pub kernel_version: String,

    /// MachineID reported by the node. For unique machine identification in the cluster this field is preferred. Learn more from man(5) machine-id: http://man7.org/linux/man-pages/man5/machine-id.5.html
    pub machine_id: String,

    /// The Operating System reported by the node
    pub operating_system: String,

    //// SystemUUID reported by the node. For unique machine identification MachineID is preferred. This field is specific to Red Hat hosts https://access.redhat.com/documentation/en-us/red_hat_subscription_management/1/html/rhsm/uuid
    pub system_uuid: String,

    /// Capacity represents the total resources of a node
    pub capacity: std::collections::BTreeMap<String, Quantity>,
}

pub struct QNodeInner {}
pub enum FuncDef {
    PythonFuncDef(PythonFuncDef),
}

pub struct PythonFuncDef {
    pub environment: String,
    pub envs: Vec<(String, String)>,
    pub workingDir: Option<String>,
    pub funcName: String,
    pub initArgments: String,

    pub resourceReq: BTreeMap<String, Quantity>,
}

pub struct Environment {
    pub image: String,
    pub envs: BTreeMap<String, String>,
    pub commands: Vec<String>,
    pub args: Vec<String>,
    pub working_dir: String,
    pub volume_mounts: Vec<VolumeMount>,

    pub overhead: BTreeMap<String, Quantity>,
}

pub struct EnvDeployment {
    pub environment: String,
    pub resource: BTreeMap<String, Quantity>,
}

pub struct FuncServiceSpec {
    pub environments: BTreeMap<String, Environment>,
    pub functions: BTreeMap<String, FuncDef>,
    pub httpEntryFunc: String, // entry function name
}

pub struct FuncServiceDeployConfig {
    pub envDeployments: BTreeMap<String, EnvDeployment>, // envDeployName --> EnvDeployment
    pub funcMapping: BTreeMap<String, String>,           // funcName --> PodName
}

pub struct FuncServiceDeployment {
    pub envDeployments: BTreeMap<String, FuncPod>, // podname --> PodDef
}

pub struct FuncServiceInstance {}

pub type ReturnId = u64;

#[derive(Debug, Clone, Copy)]
pub enum WorkerPodState {
    Init,
    Working(i64),   // leased by one gateway, GatewayId
    Idle(ReturnId), // no one leasing the worker, inner is return SeqId,
}

impl WorkerPodState {
    pub fn IsIdle(&self) -> bool {
        match self {
            Self::Idle(_) => return true,
            _ => return false,
        }
    }
}


#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PodCondition {
    /// Last time we probed the condition.
    pub last_probe_time: SystemTime,

    /// Last time the condition transitioned from one status to another.
    pub last_transition_time: SystemTime,

    /// Human-readable message indicating details about last transition.
    pub message: String,

    /// Unique, one-word, CamelCase reason for the condition's last transition.
    pub reason: String,

    /// Status is the status of the condition. Can be True, False, Unknown. More info: https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle#pod-conditions
    pub status: String,

    /// Type is the type of the condition. More info: https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle#pod-conditions
    pub type_: String,
}

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct PodStatus {
    pub host_ip: String,
    pub pod_ip: String,
    pub pod_ips: Vec<String>,
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
