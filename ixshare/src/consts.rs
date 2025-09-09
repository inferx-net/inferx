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

use const_format::formatcp;

pub const AnnotationNodeMgrNode: &str = "node.inferx.net";

pub const DefaultNodeMgrNodeTenant: &str = "system";
pub const DefaultNodeMgrNodeNameSpace: &str = "inferx";

// NodePending means the node has been created/added by the system, but not configured.
pub const NodePending: &str = "Pending";
// NodeRunning means the node has been configured and has Kubernetes components running.
pub const NodeRunning: &str = "Running";
// NodeTerminated means the node has been removed from the cluster.
pub const NodeTerminated: &str = "Terminated";

// NodeReady means kubelet is healthy and ready to accept pods.
pub const NodeReady: &str = "Ready";
// NodeNetworkUnavailable means that network for the node is not correctly configured.
pub const NodeNetworkUnavailable: &str = "NetworkUnavailable";

// These are valid condition statuses. "ConditionTrue" means a resource is in the condition.
// "ConditionFalse" means a resource is not in the condition. "ConditionUnknown" means kubernetes
// can't decide if a resource is in the condition or not. In the future, we could add other
// intermediate conditions, e.g. ConditionDegraded.
pub const ConditionTrue: &str = "True";
pub const ConditionFalse: &str = "False";
pub const ConditionUnknown: &str = "Unknown";

pub const BASE_DIR: &str = "/opt/inferx/";
pub const SQL_PATH: &'static str = formatcp!("{}/data", BASE_DIR);
pub const SQL_DATA: &'static str = formatcp!("{}/sqlite.db", SQL_PATH);

pub const DEFAULT_PODMGR_PORT: u16 = 1233;
pub const DEFAULT_TSOTCNI_PORT: u16 = 1234;
pub const DEFAULT_TSOTSVC_PORT: u16 = 1235;
pub const DEFAULT_NASTATESVC_PORT: u16 = 1236;
pub const DEFAULT_STATESVC_PORT: u16 = 1237;
pub const DEFAULT_SCHEDULER_PORT: u16 = 1238;
pub const DEFAULT_GATEWAY_PORT: u16 = 4000;
pub const DEFAULT_DASHBOARD_PORT: u16 = 1239;

pub static TSOT_SOCKET_PATH: &'static str = "/opt/inferx/sockets/tsot-socket";
pub static TSOT_GW_SOCKET_PATH: &'static str = "/opt/inferx/sockets_host/tsot-socket";
pub const AUDITDB_ADDR: &str = "postgresql://audit_user:123456@localhost/auditdb";
