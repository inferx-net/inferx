use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::data_obj::*;

/// NodeStatusSpec is information about the current status of a node.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeStatusSpec {
    pub phase: String,

    /// List of addresses reachable to the node
    pub addresses: Vec<NodeAddress>,

    /// Allocatable represents the resources of a node that are available for scheduling. Defaults to Capacity.
    pub allocatable: BTreeMap<String, Quantity>,

    /// Capacity represents the total resources of a node
    pub capacity: std::collections::BTreeMap<String, Quantity>,

    /// Conditions is an array of current observed node conditions.
    pub conditions: Vec<NodeCondition>,

    /// List of container images on this node
    pub images: Vec<ContainerImage>,

    /// Set of ids/uuids to uniquely identify the node
    pub node_info: NodeSystemInfo,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeAddress {
    /// The node address.
    pub address: String,

    /// Node address type, one of Hostname, ExternalIP or InternalIP.
    pub type_: String,
}

// // CPU, in cores. (500m = .5 cores)
// pub const ResourceCPU: &str = "cpu";
// // Memory, in bytes. (500Gi = 500GiB = 500 * 1024 * 1024 * 1024)
// pub const ResourceMemory: &str = "memory";
// // Volume size, in bytes (e,g. 5Gi = 5GiB = 5 * 1024 * 1024 * 1024)
// pub const ResourceStorage: &str = "storage";
// // Local ephemeral storage, in bytes. (500Gi = 500GiB = 500 * 1024 * 1024 * 1024)
// // The resource name for ResourceEphemeralStorage is alpha and it can change across releases.
// pub const ResourceEphemeralStorage: &str = "ephemeral-storage";

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

pub type NodeStatus = DataObject<NodeStatusSpec>;
pub type NodeStatusMgr = DataObjectMgr<NodeStatusSpec>;

impl NodeStatus {
    pub const KEY: &'static str = "node";
    pub const TENANT: &'static str = "system";
    pub const NAMESPACE: &'static str = "system";

    pub fn NodeId(&self) -> String {
        return format!("{}/{}/{}", &self.tenant, &self.namespace, &self.name);
    }
}
