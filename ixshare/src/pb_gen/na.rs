#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreatePodSandboxReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(string, tag = "5")]
    pub id: ::prost::alloc::string::String,
    #[prost(string, tag = "6")]
    pub uid: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreatePodSandboxResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(int32, tag = "2")]
    pub ipaddr: i32,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemovePodSandboxReq {
    #[prost(string, tag = "1")]
    pub uid: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemovePodSandboxResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LoadSnapshotReq {
    #[prost(string, tag = "1")]
    pub funckey: ::prost::alloc::string::String,
    #[prost(int64, tag = "2")]
    pub gpu_standby: i64,
    #[prost(int64, tag = "3")]
    pub pageable_standby: i64,
    #[prost(int64, tag = "4")]
    pub pinned_standby: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LoadSnapshotResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoveSnapshotReq {
    #[prost(string, tag = "1")]
    pub funckey: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RemoveSnapshotResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WorkerId {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(string, tag = "5")]
    pub id: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ConnectReq {
    #[prost(int64, tag = "1")]
    pub gateway_id: i64,
    #[prost(message, repeated, tag = "2")]
    pub workers: ::prost::alloc::vec::Vec<WorkerId>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ConnectResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LeaseWorkerReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(int64, tag = "5")]
    pub gateway_id: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct LeaseWorkerResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub id: ::prost::alloc::string::String,
    #[prost(uint32, tag = "4")]
    pub ipaddr: u32,
    #[prost(bool, tag = "5")]
    pub keepalive: bool,
    #[prost(uint32, tag = "6")]
    pub hostipaddr: u32,
    #[prost(uint32, tag = "7")]
    pub hostport: u32,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ReturnWorkerReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(string, tag = "5")]
    pub id: ::prost::alloc::string::String,
    #[prost(bool, tag = "6")]
    pub failworker: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ReturnWorkerResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RefreshGatewayReq {
    #[prost(int64, tag = "1")]
    pub gateway_id: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RefreshGatewayResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeAgentRespMsg {
    #[prost(oneof = "node_agent_resp_msg::MessageBody", tags = "100, 200")]
    pub message_body: ::core::option::Option<node_agent_resp_msg::MessageBody>,
}
/// Nested message and enum types in `NodeAgentRespMsg`.
pub mod node_agent_resp_msg {
    #[derive(serde::Serialize, serde::Deserialize)]
    #[allow(clippy::derive_partial_eq_without_eq)]
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum MessageBody {
        #[prost(message, tag = "100")]
        NodeAgentResp(super::NodeAgentResp),
        #[prost(message, tag = "200")]
        NodeAgentStreamMsg(super::NodeAgentStreamMsg),
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeAgentReq {
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(oneof = "node_agent_req::MessageBody", tags = "100, 200, 300, 400")]
    pub message_body: ::core::option::Option<node_agent_req::MessageBody>,
}
/// Nested message and enum types in `NodeAgentReq`.
pub mod node_agent_req {
    #[derive(serde::Serialize, serde::Deserialize)]
    #[allow(clippy::derive_partial_eq_without_eq)]
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum MessageBody {
        #[prost(message, tag = "100")]
        NodeConfigReq(super::NodeConfigReq),
        #[prost(message, tag = "200")]
        CreatePodReq(super::CreatePodReq),
        #[prost(message, tag = "300")]
        TerminatePodReq(super::TerminatePodReq),
        #[prost(message, tag = "400")]
        ReadFuncLogReq(super::ReadFuncLogReq),
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeAgentResp {
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(string, tag = "2")]
    pub error: ::prost::alloc::string::String,
    #[prost(oneof = "node_agent_resp::MessageBody", tags = "100, 200, 300, 400")]
    pub message_body: ::core::option::Option<node_agent_resp::MessageBody>,
}
/// Nested message and enum types in `NodeAgentResp`.
pub mod node_agent_resp {
    #[derive(serde::Serialize, serde::Deserialize)]
    #[allow(clippy::derive_partial_eq_without_eq)]
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum MessageBody {
        #[prost(message, tag = "100")]
        NodeConfigResp(super::NodeConfigResp),
        #[prost(message, tag = "200")]
        CreatePodResp(super::CreatePodResp),
        #[prost(message, tag = "300")]
        TerminatePodResp(super::TerminatePodResp),
        #[prost(message, tag = "400")]
        ReadFuncLogResp(super::ReadFuncLogResp),
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Env {
    #[prost(string, tag = "1")]
    pub name: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub value: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Mount {
    #[prost(string, tag = "1")]
    pub host_path: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub mount_path: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ContainerPort {
    #[prost(int32, tag = "1")]
    pub host_port: i32,
    #[prost(int32, tag = "2")]
    pub container_port: i32,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Kv {
    #[prost(string, tag = "1")]
    pub key: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub val: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreateFuncPodReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(string, tag = "5")]
    pub id: ::prost::alloc::string::String,
    #[prost(message, repeated, tag = "6")]
    pub labels: ::prost::alloc::vec::Vec<Kv>,
    #[prost(message, repeated, tag = "7")]
    pub annotations: ::prost::alloc::vec::Vec<Kv>,
    #[prost(enumeration = "CreatePodType", tag = "8")]
    pub create_type: i32,
    #[prost(string, tag = "9")]
    pub funcspec: ::prost::alloc::string::String,
    #[prost(string, tag = "10")]
    pub alloc_resources: ::prost::alloc::string::String,
    #[prost(string, tag = "11")]
    pub resource_quota: ::prost::alloc::string::String,
    #[prost(message, repeated, tag = "12")]
    pub terminate_pods: ::prost::alloc::vec::Vec<TerminatePodReq>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreateFuncPodResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(uint32, tag = "2")]
    pub ipaddress: u32,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FuncPodSpec {
    #[prost(string, tag = "4")]
    pub image: ::prost::alloc::string::String,
    #[prost(string, repeated, tag = "7")]
    pub commands: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(message, repeated, tag = "8")]
    pub envs: ::prost::alloc::vec::Vec<Env>,
    #[prost(message, repeated, tag = "9")]
    pub mounts: ::prost::alloc::vec::Vec<Mount>,
    #[prost(message, repeated, tag = "10")]
    pub ports: ::prost::alloc::vec::Vec<ContainerPort>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FuncPodDeploy {
    #[prost(string, tag = "1")]
    pub name: ::prost::alloc::string::String,
    #[prost(uint32, tag = "2")]
    pub ipaddress: u32,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FuncReplicas {
    #[prost(string, tag = "1")]
    pub name: ::prost::alloc::string::String,
    #[prost(int32, tag = "2")]
    pub replica: i32,
    #[prost(int32, tag = "3")]
    pub min_replica: i32,
    #[prost(oneof = "func_replicas::Set", tags = "100, 200")]
    pub set: ::core::option::Option<func_replicas::Set>,
}
/// Nested message and enum types in `FuncReplicas`.
pub mod func_replicas {
    #[derive(serde::Serialize, serde::Deserialize)]
    #[allow(clippy::derive_partial_eq_without_eq)]
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Set {
        #[prost(message, tag = "100")]
        Spec(super::FuncPodSpec),
        #[prost(message, tag = "200")]
        Set(::prost::alloc::boxed::Box<super::FuncReplicas>),
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FuncReplicasDeploy {
    #[prost(string, tag = "1")]
    pub name: ::prost::alloc::string::String,
    #[prost(message, repeated, tag = "2")]
    pub pods: ::prost::alloc::vec::Vec<FuncPodDeploy>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreateFuncServiceReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub name: ::prost::alloc::string::String,
    #[prost(message, repeated, tag = "5")]
    pub labels: ::prost::alloc::vec::Vec<Kv>,
    #[prost(message, repeated, tag = "6")]
    pub annotations: ::prost::alloc::vec::Vec<Kv>,
    #[prost(message, repeated, tag = "7")]
    pub sets: ::prost::alloc::vec::Vec<FuncReplicas>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreateFuncServiceResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(message, repeated, tag = "2")]
    pub replicas: ::prost::alloc::vec::Vec<FuncReplicasDeploy>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ReadFuncLogReq {
    #[prost(string, tag = "1")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(uint64, tag = "3")]
    pub offset: u64,
    #[prost(uint32, tag = "4")]
    pub len: u32,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ReadFuncLogResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub content: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeConfigReq {
    #[prost(string, tag = "1")]
    pub cluster_domain: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub node: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeConfigResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreatePodReq {
    #[prost(string, tag = "1")]
    pub pod: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub config_map: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CreatePodResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TerminatePodReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(string, tag = "5")]
    pub id: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TerminatePodResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SnapshotPodReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(string, tag = "5")]
    pub id: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SnapshotPodResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct HibernatePodReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(string, tag = "5")]
    pub id: ::prost::alloc::string::String,
    /// 1: GPU 2: Disk
    #[prost(uint32, tag = "6")]
    pub hibernate_type: u32,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct HibernatePodResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ReadPodLogReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(string, tag = "5")]
    pub id: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ReadPodLogResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub log: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WakeupPodReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(string, tag = "5")]
    pub id: ::prost::alloc::string::String,
    /// 1: GPU 2: Disk
    #[prost(uint32, tag = "6")]
    pub hibernate_type: u32,
    #[prost(string, tag = "7")]
    pub alloc_resources: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct WakeupPodResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ResumePodReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(int64, tag = "4")]
    pub fprevision: i64,
    #[prost(string, tag = "5")]
    pub id: ::prost::alloc::string::String,
    #[prost(string, tag = "7")]
    pub alloc_resources: ::prost::alloc::string::String,
    #[prost(message, repeated, tag = "8")]
    pub terminate_pods: ::prost::alloc::vec::Vec<TerminatePodReq>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ResumePodResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GetPodReq {
    #[prost(string, tag = "1")]
    pub tenant: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub namespace: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub funcname: ::prost::alloc::string::String,
    #[prost(string, tag = "4")]
    pub id: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GetPodResp {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub pod: ::prost::alloc::string::String,
    #[prost(int64, tag = "3")]
    pub revision: i64,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeAgentStreamMsg {
    #[prost(oneof = "node_agent_stream_msg::EventBody", tags = "100, 200, 300")]
    pub event_body: ::core::option::Option<node_agent_stream_msg::EventBody>,
}
/// Nested message and enum types in `NodeAgentStreamMsg`.
pub mod node_agent_stream_msg {
    #[derive(serde::Serialize, serde::Deserialize)]
    #[allow(clippy::derive_partial_eq_without_eq)]
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum EventBody {
        #[prost(message, tag = "100")]
        NodeRegister(super::NodeRegister),
        #[prost(message, tag = "200")]
        NodeUpdate(super::NodeUpdate),
        #[prost(message, tag = "300")]
        PodEvent(super::PodEvent),
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeRegister {
    #[prost(int64, tag = "2")]
    pub revision: i64,
    #[prost(string, tag = "3")]
    pub node: ::prost::alloc::string::String,
    #[prost(string, repeated, tag = "4")]
    pub pods: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct NodeUpdate {
    #[prost(int64, tag = "2")]
    pub revision: i64,
    #[prost(string, tag = "3")]
    pub node: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PodEvent {
    #[prost(enumeration = "EventType", tag = "1")]
    pub event_type: i32,
    #[prost(int64, tag = "2")]
    pub revision: i64,
    #[prost(string, tag = "3")]
    pub pod: ::prost::alloc::string::String,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum CreatePodType {
    Normal = 0,
    Snapshot = 2,
    Restore = 3,
}
impl CreatePodType {
    /// String value of the enum field names used in the ProtoBuf definition.
    ///
    /// The values are not transformed in any way and thus are considered stable
    /// (if the ProtoBuf definition does not change) and safe for programmatic use.
    pub fn as_str_name(&self) -> &'static str {
        match self {
            CreatePodType::Normal => "Normal",
            CreatePodType::Snapshot => "Snapshot",
            CreatePodType::Restore => "Restore",
        }
    }
    /// Creates an enum from field names used in the ProtoBuf definition.
    pub fn from_str_name(value: &str) -> ::core::option::Option<Self> {
        match value {
            "Normal" => Some(Self::Normal),
            "Snapshot" => Some(Self::Snapshot),
            "Restore" => Some(Self::Restore),
            _ => None,
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum EventType {
    Add = 0,
    Update = 2,
    Delete = 3,
}
impl EventType {
    /// String value of the enum field names used in the ProtoBuf definition.
    ///
    /// The values are not transformed in any way and thus are considered stable
    /// (if the ProtoBuf definition does not change) and safe for programmatic use.
    pub fn as_str_name(&self) -> &'static str {
        match self {
            EventType::Add => "Add",
            EventType::Update => "Update",
            EventType::Delete => "Delete",
        }
    }
    /// Creates an enum from field names used in the ProtoBuf definition.
    pub fn from_str_name(value: &str) -> ::core::option::Option<Self> {
        match value {
            "Add" => Some(Self::Add),
            "Update" => Some(Self::Update),
            "Delete" => Some(Self::Delete),
            _ => None,
        }
    }
}
/// Generated client implementations.
pub mod scheduler_service_client {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    use tonic::codegen::http::Uri;
    #[derive(Debug, Clone)]
    pub struct SchedulerServiceClient<T> {
        inner: tonic::client::Grpc<T>,
    }
    impl SchedulerServiceClient<tonic::transport::Channel> {
        /// Attempt to create a new client by connecting to a given endpoint.
        pub async fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
        where
            D: std::convert::TryInto<tonic::transport::Endpoint>,
            D::Error: Into<StdError>,
        {
            let conn = tonic::transport::Endpoint::new(dst)?.connect().await?;
            Ok(Self::new(conn))
        }
    }
    impl<T> SchedulerServiceClient<T>
    where
        T: tonic::client::GrpcService<tonic::body::BoxBody>,
        T::Error: Into<StdError>,
        T::ResponseBody: Body<Data = Bytes> + Send + 'static,
        <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    {
        pub fn new(inner: T) -> Self {
            let inner = tonic::client::Grpc::new(inner);
            Self { inner }
        }
        pub fn with_origin(inner: T, origin: Uri) -> Self {
            let inner = tonic::client::Grpc::with_origin(inner, origin);
            Self { inner }
        }
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> SchedulerServiceClient<InterceptedService<T, F>>
        where
            F: tonic::service::Interceptor,
            T::ResponseBody: Default,
            T: tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
                Response = http::Response<
                    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::ResponseBody,
                >,
            >,
            <T as tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
            >>::Error: Into<StdError> + Send + Sync,
        {
            SchedulerServiceClient::new(InterceptedService::new(inner, interceptor))
        }
        /// Compress requests with the given encoding.
        ///
        /// This requires the server to support it otherwise it might respond with an
        /// error.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.inner = self.inner.send_compressed(encoding);
            self
        }
        /// Enable decompressing responses.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.inner = self.inner.accept_compressed(encoding);
            self
        }
        pub async fn connect_scheduler(
            &mut self,
            request: impl tonic::IntoRequest<super::ConnectReq>,
        ) -> Result<tonic::Response<super::ConnectResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.SchedulerService/ConnectScheduler",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn lease_worker(
            &mut self,
            request: impl tonic::IntoRequest<super::LeaseWorkerReq>,
        ) -> Result<tonic::Response<super::LeaseWorkerResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.SchedulerService/LeaseWorker",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn return_worker(
            &mut self,
            request: impl tonic::IntoRequest<super::ReturnWorkerReq>,
        ) -> Result<tonic::Response<super::ReturnWorkerResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.SchedulerService/ReturnWorker",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn refresh_gateway(
            &mut self,
            request: impl tonic::IntoRequest<super::RefreshGatewayReq>,
        ) -> Result<tonic::Response<super::RefreshGatewayResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.SchedulerService/RefreshGateway",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
    }
}
/// Generated client implementations.
pub mod node_agent_service_client {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    use tonic::codegen::http::Uri;
    #[derive(Debug, Clone)]
    pub struct NodeAgentServiceClient<T> {
        inner: tonic::client::Grpc<T>,
    }
    impl NodeAgentServiceClient<tonic::transport::Channel> {
        /// Attempt to create a new client by connecting to a given endpoint.
        pub async fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
        where
            D: std::convert::TryInto<tonic::transport::Endpoint>,
            D::Error: Into<StdError>,
        {
            let conn = tonic::transport::Endpoint::new(dst)?.connect().await?;
            Ok(Self::new(conn))
        }
    }
    impl<T> NodeAgentServiceClient<T>
    where
        T: tonic::client::GrpcService<tonic::body::BoxBody>,
        T::Error: Into<StdError>,
        T::ResponseBody: Body<Data = Bytes> + Send + 'static,
        <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    {
        pub fn new(inner: T) -> Self {
            let inner = tonic::client::Grpc::new(inner);
            Self { inner }
        }
        pub fn with_origin(inner: T, origin: Uri) -> Self {
            let inner = tonic::client::Grpc::with_origin(inner, origin);
            Self { inner }
        }
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> NodeAgentServiceClient<InterceptedService<T, F>>
        where
            F: tonic::service::Interceptor,
            T::ResponseBody: Default,
            T: tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
                Response = http::Response<
                    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::ResponseBody,
                >,
            >,
            <T as tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
            >>::Error: Into<StdError> + Send + Sync,
        {
            NodeAgentServiceClient::new(InterceptedService::new(inner, interceptor))
        }
        /// Compress requests with the given encoding.
        ///
        /// This requires the server to support it otherwise it might respond with an
        /// error.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.inner = self.inner.send_compressed(encoding);
            self
        }
        /// Enable decompressing responses.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.inner = self.inner.accept_compressed(encoding);
            self
        }
        pub async fn create_func_pod(
            &mut self,
            request: impl tonic::IntoRequest<super::CreateFuncPodReq>,
        ) -> Result<tonic::Response<super::CreateFuncPodResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.NodeAgentService/CreateFuncPod",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn terminate_pod(
            &mut self,
            request: impl tonic::IntoRequest<super::TerminatePodReq>,
        ) -> Result<tonic::Response<super::TerminatePodResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.NodeAgentService/TerminatePod",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn resume_pod(
            &mut self,
            request: impl tonic::IntoRequest<super::ResumePodReq>,
        ) -> Result<tonic::Response<super::ResumePodResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.NodeAgentService/ResumePod",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn read_pod_log(
            &mut self,
            request: impl tonic::IntoRequest<super::ReadPodLogReq>,
        ) -> Result<tonic::Response<super::ReadPodLogResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.NodeAgentService/ReadPodLog",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn load_snapshot(
            &mut self,
            request: impl tonic::IntoRequest<super::LoadSnapshotReq>,
        ) -> Result<tonic::Response<super::LoadSnapshotResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.NodeAgentService/LoadSnapshot",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn remove_snapshot(
            &mut self,
            request: impl tonic::IntoRequest<super::RemoveSnapshotReq>,
        ) -> Result<tonic::Response<super::RemoveSnapshotResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.NodeAgentService/RemoveSnapshot",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
    }
}
/// Generated client implementations.
pub mod ix_proxy_service_client {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    use tonic::codegen::http::Uri;
    #[derive(Debug, Clone)]
    pub struct IxProxyServiceClient<T> {
        inner: tonic::client::Grpc<T>,
    }
    impl IxProxyServiceClient<tonic::transport::Channel> {
        /// Attempt to create a new client by connecting to a given endpoint.
        pub async fn connect<D>(dst: D) -> Result<Self, tonic::transport::Error>
        where
            D: std::convert::TryInto<tonic::transport::Endpoint>,
            D::Error: Into<StdError>,
        {
            let conn = tonic::transport::Endpoint::new(dst)?.connect().await?;
            Ok(Self::new(conn))
        }
    }
    impl<T> IxProxyServiceClient<T>
    where
        T: tonic::client::GrpcService<tonic::body::BoxBody>,
        T::Error: Into<StdError>,
        T::ResponseBody: Body<Data = Bytes> + Send + 'static,
        <T::ResponseBody as Body>::Error: Into<StdError> + Send,
    {
        pub fn new(inner: T) -> Self {
            let inner = tonic::client::Grpc::new(inner);
            Self { inner }
        }
        pub fn with_origin(inner: T, origin: Uri) -> Self {
            let inner = tonic::client::Grpc::with_origin(inner, origin);
            Self { inner }
        }
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> IxProxyServiceClient<InterceptedService<T, F>>
        where
            F: tonic::service::Interceptor,
            T::ResponseBody: Default,
            T: tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
                Response = http::Response<
                    <T as tonic::client::GrpcService<tonic::body::BoxBody>>::ResponseBody,
                >,
            >,
            <T as tonic::codegen::Service<
                http::Request<tonic::body::BoxBody>,
            >>::Error: Into<StdError> + Send + Sync,
        {
            IxProxyServiceClient::new(InterceptedService::new(inner, interceptor))
        }
        /// Compress requests with the given encoding.
        ///
        /// This requires the server to support it otherwise it might respond with an
        /// error.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.inner = self.inner.send_compressed(encoding);
            self
        }
        /// Enable decompressing responses.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.inner = self.inner.accept_compressed(encoding);
            self
        }
        pub async fn create_pod_sandbox(
            &mut self,
            request: impl tonic::IntoRequest<super::CreatePodSandboxReq>,
        ) -> Result<tonic::Response<super::CreatePodSandboxResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.IxProxyService/CreatePodSandbox",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
        pub async fn remove_pod_sandbox(
            &mut self,
            request: impl tonic::IntoRequest<super::RemovePodSandboxReq>,
        ) -> Result<tonic::Response<super::RemovePodSandboxResp>, tonic::Status> {
            self.inner
                .ready()
                .await
                .map_err(|e| {
                    tonic::Status::new(
                        tonic::Code::Unknown,
                        format!("Service was not ready: {}", e.into()),
                    )
                })?;
            let codec = tonic::codec::ProstCodec::default();
            let path = http::uri::PathAndQuery::from_static(
                "/na.IxProxyService/RemovePodSandbox",
            );
            self.inner.unary(request.into_request(), path, codec).await
        }
    }
}
/// Generated server implementations.
pub mod scheduler_service_server {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    /// Generated trait containing gRPC methods that should be implemented for use with SchedulerServiceServer.
    #[async_trait]
    pub trait SchedulerService: Send + Sync + 'static {
        async fn connect_scheduler(
            &self,
            request: tonic::Request<super::ConnectReq>,
        ) -> Result<tonic::Response<super::ConnectResp>, tonic::Status>;
        async fn lease_worker(
            &self,
            request: tonic::Request<super::LeaseWorkerReq>,
        ) -> Result<tonic::Response<super::LeaseWorkerResp>, tonic::Status>;
        async fn return_worker(
            &self,
            request: tonic::Request<super::ReturnWorkerReq>,
        ) -> Result<tonic::Response<super::ReturnWorkerResp>, tonic::Status>;
        async fn refresh_gateway(
            &self,
            request: tonic::Request<super::RefreshGatewayReq>,
        ) -> Result<tonic::Response<super::RefreshGatewayResp>, tonic::Status>;
    }
    #[derive(Debug)]
    pub struct SchedulerServiceServer<T: SchedulerService> {
        inner: _Inner<T>,
        accept_compression_encodings: EnabledCompressionEncodings,
        send_compression_encodings: EnabledCompressionEncodings,
    }
    struct _Inner<T>(Arc<T>);
    impl<T: SchedulerService> SchedulerServiceServer<T> {
        pub fn new(inner: T) -> Self {
            Self::from_arc(Arc::new(inner))
        }
        pub fn from_arc(inner: Arc<T>) -> Self {
            let inner = _Inner(inner);
            Self {
                inner,
                accept_compression_encodings: Default::default(),
                send_compression_encodings: Default::default(),
            }
        }
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> InterceptedService<Self, F>
        where
            F: tonic::service::Interceptor,
        {
            InterceptedService::new(Self::new(inner), interceptor)
        }
        /// Enable decompressing requests with the given encoding.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.accept_compression_encodings.enable(encoding);
            self
        }
        /// Compress responses with the given encoding, if the client supports it.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.send_compression_encodings.enable(encoding);
            self
        }
    }
    impl<T, B> tonic::codegen::Service<http::Request<B>> for SchedulerServiceServer<T>
    where
        T: SchedulerService,
        B: Body + Send + 'static,
        B::Error: Into<StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::BoxBody>;
        type Error = std::convert::Infallible;
        type Future = BoxFuture<Self::Response, Self::Error>;
        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            let inner = self.inner.clone();
            match req.uri().path() {
                "/na.SchedulerService/ConnectScheduler" => {
                    #[allow(non_camel_case_types)]
                    struct ConnectSchedulerSvc<T: SchedulerService>(pub Arc<T>);
                    impl<
                        T: SchedulerService,
                    > tonic::server::UnaryService<super::ConnectReq>
                    for ConnectSchedulerSvc<T> {
                        type Response = super::ConnectResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::ConnectReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).connect_scheduler(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = ConnectSchedulerSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/na.SchedulerService/LeaseWorker" => {
                    #[allow(non_camel_case_types)]
                    struct LeaseWorkerSvc<T: SchedulerService>(pub Arc<T>);
                    impl<
                        T: SchedulerService,
                    > tonic::server::UnaryService<super::LeaseWorkerReq>
                    for LeaseWorkerSvc<T> {
                        type Response = super::LeaseWorkerResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::LeaseWorkerReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).lease_worker(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = LeaseWorkerSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/na.SchedulerService/ReturnWorker" => {
                    #[allow(non_camel_case_types)]
                    struct ReturnWorkerSvc<T: SchedulerService>(pub Arc<T>);
                    impl<
                        T: SchedulerService,
                    > tonic::server::UnaryService<super::ReturnWorkerReq>
                    for ReturnWorkerSvc<T> {
                        type Response = super::ReturnWorkerResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::ReturnWorkerReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).return_worker(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = ReturnWorkerSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/na.SchedulerService/RefreshGateway" => {
                    #[allow(non_camel_case_types)]
                    struct RefreshGatewaySvc<T: SchedulerService>(pub Arc<T>);
                    impl<
                        T: SchedulerService,
                    > tonic::server::UnaryService<super::RefreshGatewayReq>
                    for RefreshGatewaySvc<T> {
                        type Response = super::RefreshGatewayResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::RefreshGatewayReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).refresh_gateway(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = RefreshGatewaySvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                _ => {
                    Box::pin(async move {
                        Ok(
                            http::Response::builder()
                                .status(200)
                                .header("grpc-status", "12")
                                .header("content-type", "application/grpc")
                                .body(empty_body())
                                .unwrap(),
                        )
                    })
                }
            }
        }
    }
    impl<T: SchedulerService> Clone for SchedulerServiceServer<T> {
        fn clone(&self) -> Self {
            let inner = self.inner.clone();
            Self {
                inner,
                accept_compression_encodings: self.accept_compression_encodings,
                send_compression_encodings: self.send_compression_encodings,
            }
        }
    }
    impl<T: SchedulerService> Clone for _Inner<T> {
        fn clone(&self) -> Self {
            Self(self.0.clone())
        }
    }
    impl<T: std::fmt::Debug> std::fmt::Debug for _Inner<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", self.0)
        }
    }
    impl<T: SchedulerService> tonic::server::NamedService for SchedulerServiceServer<T> {
        const NAME: &'static str = "na.SchedulerService";
    }
}
/// Generated server implementations.
pub mod node_agent_service_server {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    /// Generated trait containing gRPC methods that should be implemented for use with NodeAgentServiceServer.
    #[async_trait]
    pub trait NodeAgentService: Send + Sync + 'static {
        async fn create_func_pod(
            &self,
            request: tonic::Request<super::CreateFuncPodReq>,
        ) -> Result<tonic::Response<super::CreateFuncPodResp>, tonic::Status>;
        async fn terminate_pod(
            &self,
            request: tonic::Request<super::TerminatePodReq>,
        ) -> Result<tonic::Response<super::TerminatePodResp>, tonic::Status>;
        async fn resume_pod(
            &self,
            request: tonic::Request<super::ResumePodReq>,
        ) -> Result<tonic::Response<super::ResumePodResp>, tonic::Status>;
        async fn read_pod_log(
            &self,
            request: tonic::Request<super::ReadPodLogReq>,
        ) -> Result<tonic::Response<super::ReadPodLogResp>, tonic::Status>;
        async fn load_snapshot(
            &self,
            request: tonic::Request<super::LoadSnapshotReq>,
        ) -> Result<tonic::Response<super::LoadSnapshotResp>, tonic::Status>;
        async fn remove_snapshot(
            &self,
            request: tonic::Request<super::RemoveSnapshotReq>,
        ) -> Result<tonic::Response<super::RemoveSnapshotResp>, tonic::Status>;
    }
    #[derive(Debug)]
    pub struct NodeAgentServiceServer<T: NodeAgentService> {
        inner: _Inner<T>,
        accept_compression_encodings: EnabledCompressionEncodings,
        send_compression_encodings: EnabledCompressionEncodings,
    }
    struct _Inner<T>(Arc<T>);
    impl<T: NodeAgentService> NodeAgentServiceServer<T> {
        pub fn new(inner: T) -> Self {
            Self::from_arc(Arc::new(inner))
        }
        pub fn from_arc(inner: Arc<T>) -> Self {
            let inner = _Inner(inner);
            Self {
                inner,
                accept_compression_encodings: Default::default(),
                send_compression_encodings: Default::default(),
            }
        }
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> InterceptedService<Self, F>
        where
            F: tonic::service::Interceptor,
        {
            InterceptedService::new(Self::new(inner), interceptor)
        }
        /// Enable decompressing requests with the given encoding.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.accept_compression_encodings.enable(encoding);
            self
        }
        /// Compress responses with the given encoding, if the client supports it.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.send_compression_encodings.enable(encoding);
            self
        }
    }
    impl<T, B> tonic::codegen::Service<http::Request<B>> for NodeAgentServiceServer<T>
    where
        T: NodeAgentService,
        B: Body + Send + 'static,
        B::Error: Into<StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::BoxBody>;
        type Error = std::convert::Infallible;
        type Future = BoxFuture<Self::Response, Self::Error>;
        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            let inner = self.inner.clone();
            match req.uri().path() {
                "/na.NodeAgentService/CreateFuncPod" => {
                    #[allow(non_camel_case_types)]
                    struct CreateFuncPodSvc<T: NodeAgentService>(pub Arc<T>);
                    impl<
                        T: NodeAgentService,
                    > tonic::server::UnaryService<super::CreateFuncPodReq>
                    for CreateFuncPodSvc<T> {
                        type Response = super::CreateFuncPodResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::CreateFuncPodReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).create_func_pod(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = CreateFuncPodSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/na.NodeAgentService/TerminatePod" => {
                    #[allow(non_camel_case_types)]
                    struct TerminatePodSvc<T: NodeAgentService>(pub Arc<T>);
                    impl<
                        T: NodeAgentService,
                    > tonic::server::UnaryService<super::TerminatePodReq>
                    for TerminatePodSvc<T> {
                        type Response = super::TerminatePodResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::TerminatePodReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).terminate_pod(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = TerminatePodSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/na.NodeAgentService/ResumePod" => {
                    #[allow(non_camel_case_types)]
                    struct ResumePodSvc<T: NodeAgentService>(pub Arc<T>);
                    impl<
                        T: NodeAgentService,
                    > tonic::server::UnaryService<super::ResumePodReq>
                    for ResumePodSvc<T> {
                        type Response = super::ResumePodResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::ResumePodReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move { (*inner).resume_pod(request).await };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = ResumePodSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/na.NodeAgentService/ReadPodLog" => {
                    #[allow(non_camel_case_types)]
                    struct ReadPodLogSvc<T: NodeAgentService>(pub Arc<T>);
                    impl<
                        T: NodeAgentService,
                    > tonic::server::UnaryService<super::ReadPodLogReq>
                    for ReadPodLogSvc<T> {
                        type Response = super::ReadPodLogResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::ReadPodLogReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).read_pod_log(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = ReadPodLogSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/na.NodeAgentService/LoadSnapshot" => {
                    #[allow(non_camel_case_types)]
                    struct LoadSnapshotSvc<T: NodeAgentService>(pub Arc<T>);
                    impl<
                        T: NodeAgentService,
                    > tonic::server::UnaryService<super::LoadSnapshotReq>
                    for LoadSnapshotSvc<T> {
                        type Response = super::LoadSnapshotResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::LoadSnapshotReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).load_snapshot(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = LoadSnapshotSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/na.NodeAgentService/RemoveSnapshot" => {
                    #[allow(non_camel_case_types)]
                    struct RemoveSnapshotSvc<T: NodeAgentService>(pub Arc<T>);
                    impl<
                        T: NodeAgentService,
                    > tonic::server::UnaryService<super::RemoveSnapshotReq>
                    for RemoveSnapshotSvc<T> {
                        type Response = super::RemoveSnapshotResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::RemoveSnapshotReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).remove_snapshot(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = RemoveSnapshotSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                _ => {
                    Box::pin(async move {
                        Ok(
                            http::Response::builder()
                                .status(200)
                                .header("grpc-status", "12")
                                .header("content-type", "application/grpc")
                                .body(empty_body())
                                .unwrap(),
                        )
                    })
                }
            }
        }
    }
    impl<T: NodeAgentService> Clone for NodeAgentServiceServer<T> {
        fn clone(&self) -> Self {
            let inner = self.inner.clone();
            Self {
                inner,
                accept_compression_encodings: self.accept_compression_encodings,
                send_compression_encodings: self.send_compression_encodings,
            }
        }
    }
    impl<T: NodeAgentService> Clone for _Inner<T> {
        fn clone(&self) -> Self {
            Self(self.0.clone())
        }
    }
    impl<T: std::fmt::Debug> std::fmt::Debug for _Inner<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", self.0)
        }
    }
    impl<T: NodeAgentService> tonic::server::NamedService for NodeAgentServiceServer<T> {
        const NAME: &'static str = "na.NodeAgentService";
    }
}
/// Generated server implementations.
pub mod ix_proxy_service_server {
    #![allow(unused_variables, dead_code, missing_docs, clippy::let_unit_value)]
    use tonic::codegen::*;
    /// Generated trait containing gRPC methods that should be implemented for use with IxProxyServiceServer.
    #[async_trait]
    pub trait IxProxyService: Send + Sync + 'static {
        async fn create_pod_sandbox(
            &self,
            request: tonic::Request<super::CreatePodSandboxReq>,
        ) -> Result<tonic::Response<super::CreatePodSandboxResp>, tonic::Status>;
        async fn remove_pod_sandbox(
            &self,
            request: tonic::Request<super::RemovePodSandboxReq>,
        ) -> Result<tonic::Response<super::RemovePodSandboxResp>, tonic::Status>;
    }
    #[derive(Debug)]
    pub struct IxProxyServiceServer<T: IxProxyService> {
        inner: _Inner<T>,
        accept_compression_encodings: EnabledCompressionEncodings,
        send_compression_encodings: EnabledCompressionEncodings,
    }
    struct _Inner<T>(Arc<T>);
    impl<T: IxProxyService> IxProxyServiceServer<T> {
        pub fn new(inner: T) -> Self {
            Self::from_arc(Arc::new(inner))
        }
        pub fn from_arc(inner: Arc<T>) -> Self {
            let inner = _Inner(inner);
            Self {
                inner,
                accept_compression_encodings: Default::default(),
                send_compression_encodings: Default::default(),
            }
        }
        pub fn with_interceptor<F>(
            inner: T,
            interceptor: F,
        ) -> InterceptedService<Self, F>
        where
            F: tonic::service::Interceptor,
        {
            InterceptedService::new(Self::new(inner), interceptor)
        }
        /// Enable decompressing requests with the given encoding.
        #[must_use]
        pub fn accept_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.accept_compression_encodings.enable(encoding);
            self
        }
        /// Compress responses with the given encoding, if the client supports it.
        #[must_use]
        pub fn send_compressed(mut self, encoding: CompressionEncoding) -> Self {
            self.send_compression_encodings.enable(encoding);
            self
        }
    }
    impl<T, B> tonic::codegen::Service<http::Request<B>> for IxProxyServiceServer<T>
    where
        T: IxProxyService,
        B: Body + Send + 'static,
        B::Error: Into<StdError> + Send + 'static,
    {
        type Response = http::Response<tonic::body::BoxBody>;
        type Error = std::convert::Infallible;
        type Future = BoxFuture<Self::Response, Self::Error>;
        fn poll_ready(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: http::Request<B>) -> Self::Future {
            let inner = self.inner.clone();
            match req.uri().path() {
                "/na.IxProxyService/CreatePodSandbox" => {
                    #[allow(non_camel_case_types)]
                    struct CreatePodSandboxSvc<T: IxProxyService>(pub Arc<T>);
                    impl<
                        T: IxProxyService,
                    > tonic::server::UnaryService<super::CreatePodSandboxReq>
                    for CreatePodSandboxSvc<T> {
                        type Response = super::CreatePodSandboxResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::CreatePodSandboxReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).create_pod_sandbox(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = CreatePodSandboxSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                "/na.IxProxyService/RemovePodSandbox" => {
                    #[allow(non_camel_case_types)]
                    struct RemovePodSandboxSvc<T: IxProxyService>(pub Arc<T>);
                    impl<
                        T: IxProxyService,
                    > tonic::server::UnaryService<super::RemovePodSandboxReq>
                    for RemovePodSandboxSvc<T> {
                        type Response = super::RemovePodSandboxResp;
                        type Future = BoxFuture<
                            tonic::Response<Self::Response>,
                            tonic::Status,
                        >;
                        fn call(
                            &mut self,
                            request: tonic::Request<super::RemovePodSandboxReq>,
                        ) -> Self::Future {
                            let inner = self.0.clone();
                            let fut = async move {
                                (*inner).remove_pod_sandbox(request).await
                            };
                            Box::pin(fut)
                        }
                    }
                    let accept_compression_encodings = self.accept_compression_encodings;
                    let send_compression_encodings = self.send_compression_encodings;
                    let inner = self.inner.clone();
                    let fut = async move {
                        let inner = inner.0;
                        let method = RemovePodSandboxSvc(inner);
                        let codec = tonic::codec::ProstCodec::default();
                        let mut grpc = tonic::server::Grpc::new(codec)
                            .apply_compression_config(
                                accept_compression_encodings,
                                send_compression_encodings,
                            );
                        let res = grpc.unary(method, req).await;
                        Ok(res)
                    };
                    Box::pin(fut)
                }
                _ => {
                    Box::pin(async move {
                        Ok(
                            http::Response::builder()
                                .status(200)
                                .header("grpc-status", "12")
                                .header("content-type", "application/grpc")
                                .body(empty_body())
                                .unwrap(),
                        )
                    })
                }
            }
        }
    }
    impl<T: IxProxyService> Clone for IxProxyServiceServer<T> {
        fn clone(&self) -> Self {
            let inner = self.inner.clone();
            Self {
                inner,
                accept_compression_encodings: self.accept_compression_encodings,
                send_compression_encodings: self.send_compression_encodings,
            }
        }
    }
    impl<T: IxProxyService> Clone for _Inner<T> {
        fn clone(&self) -> Self {
            Self(self.0.clone())
        }
    }
    impl<T: std::fmt::Debug> std::fmt::Debug for _Inner<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{:?}", self.0)
        }
    }
    impl<T: IxProxyService> tonic::server::NamedService for IxProxyServiceServer<T> {
        const NAME: &'static str = "na.IxProxyService";
    }
}
