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

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::size_of;
use serde_derive::Deserialize;
use serde_derive::Serialize;

#[cfg(feature = "kernelalloc")]
use crate::KernelAllocator;

pub const DOCKER_IMAGE_NAME: &str = "imagename.inferx.net";
pub const SANDBOX_NAME: &str = "io.kubernetes.cri.sandbox-name";
pub const SANDBOX_UID: &str = "io.kubernetes.cri.sandbox-uid";

pub const SANDBOX_NAMESPACE_NAME: &str = "io.kubernetes.cri.sandbox-namespace";
pub const NA_SOCK_PATH: &str = "na_sockpath.inferx.net";
pub const NA_NODE: &str = "node.inferx.net";
pub const FUNCPACKAGE_KEY: &str = "func.inferx.net";
pub const GPU_STANDBY_KEY: &str = "gpustandby.inferx.net";
pub const PAGEABLE_STANDBY_KEY: &str = "pageablestandby.inferx.net";
pub const PINNED_STANDBY_KEY: &str = "pinnedstandby.inferx.net";


pub const SNAPSHOT_WORK_DIR: &str = "/opt/inferx/work";

pub const LOAD_UNIT: usize = 1 * 1024 * 1024 * 1024; // 1GB

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Standby {
    #[serde(default, rename = "gpu")]
    pub gpuMem: StandbyType,
    #[serde(default, rename = "pageable")]
    pub pageableMem: StandbyType,
    #[serde(default, rename = "pinned")]
    pub pinndMem: StandbyType,
}

impl Standby {
    pub fn GpuMem(&self) -> StandbyType {
        return self.gpuMem;
    }

    pub fn PageableMem(&self) -> StandbyType {
        // if self.pageableMem == StandbyType::Blob {
        //     return StandbyType::File;
        // }
        return self.pageableMem;
    }

    pub fn PinndMem(&self) -> StandbyType {
        return self.pinndMem;
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandbyType {
    File,
    Mem,
    Blob,
}

impl Default for StandbyType {
    fn default() -> Self {
        return Self::File;
    }
}

impl StandbyType {
    pub fn String(&self) -> String {
        match self {
            Self::Mem => "mem".to_owned(),
            Self::File => "file".to_owned(),
            Self::Blob => "blob".to_owned(),
        }
    }

    pub fn New(str: &str) -> Self {
        if str == "mem" {
            return Self::Mem;
        } else if str == "file" {
            return Self::File;
        } else if str == "blob" {
            return Self::Blob;
        } else {
            return Self::default();
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Default, Clone)]
pub struct SnapshotMeta {
    pub imagename: String,
    pub buildId: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Copy)]
pub struct ProcessGPU {
    pub gpuId: i32,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
pub enum ErrCode {
    None = 0,
    PodUidDonotExisit,
    ECONNREFUSED = 111, //
}

#[cfg(feature = "kernelalloc")]
#[repr(C)]
#[derive(Debug, Clone)]
pub struct TsotMessage {
    pub sockets: Vec<i32, KernelAllocator>,
    pub msg: TsotMsg,
}

#[cfg(not(feature = "kernelalloc"))]
#[repr(C)]
#[derive(Debug, Clone)]
pub struct TsotMessage {
    pub sockets: Vec<i32>,
    pub msg: TsotMsg,
}

#[cfg(feature = "kernelalloc")]
impl Default for TsotMessage {
    fn default() -> Self {
        return Self {
            sockets: Vec::new_in(KernelAllocator {}),
            msg: TsotMsg::None,
        };
    }
}

#[cfg(feature = "kernelalloc")]
impl From<TsotMsg> for TsotMessage {
    fn from(msg: TsotMsg) -> Self {
        return Self {
            sockets: Vec::new_in(KernelAllocator {}),
            msg: msg,
        };
    }
}

#[cfg(not(feature = "kernelalloc"))]
impl Default for TsotMessage {
    fn default() -> Self {
        return Self {
            sockets: Vec::new(),
            msg: TsotMsg::None,
        };
    }
}

#[cfg(not(feature = "kernelalloc"))]
impl From<TsotMsg> for TsotMessage {
    fn from(msg: TsotMsg) -> Self {
        return Self {
            sockets: Vec::new(),
            msg: msg,
        };
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum TsotMsg {
    None,

    //////////////////////////////////////////////////////
    PodRegisterReq(PodRegisterReq),

    // admin gateway connection, it could connect to any namespace/pod as a client
    GatewayRegisterReq(GatewayRegisterReq),

    CreateSocketReq(CreateSocketReq),

    ListenReq(ListenReq),
    AcceptReq(AcceptReq),
    StopListenReq(StopListenReq),
    PodConnectReq(PodConnectReq),
    GatewayConnectReq(GatewayConnectReq),
    DnsReq(DnsReq),
    HibereteDone(HibernateDone),
    WakeupDone(WakeupDone),
    SnapshotDone(SnapshotDone),
    Standby,
    RunningPaused,

    GPUMemoryRestoreReq(GPUMemoryRestoreReq),
    GPUMemoryOffloadReq(GPUMemoryOffloadReq),
    GPUMemoryFdsReq(GPUMemoryFdsReq),
    SnapshotDataReady(SnapshotDataReady),
    PanicInfo(PanicInfo),
    // GPUMemoryOffloadReqDone,

    //////////////////////////////////////////////////////
    PodRegisterResp(PodRegisterResp),
    GatewayRegisterResp(GatewayRegisterResp),
    CreateSocketResp(CreateSocketResp),
    PeerConnectNotify(PeerConnectNotify),
    PodConnectResp(PodConnectResp),
    GatewayConnectResp(GatewayConnectResp),
    DnsResp(DnsResp),
    Hibernate(Hibernate),
    Wakeup(Wakeup),
    Resume([u32; 8]),
    ResetGPU(bool),
    GPUMemoryOffloadReqDoneResp(GPUMemoryOffloadReqDoneResp),
    GPUMemoryRestoreReqDoneResp(GPUMemoryRestoreReqDoneResp),
    GPUMemoryFdsReqDoneResp(GPUMemoryFdsReqDoneResp),
    Snapshot,
    GPUMemoryObjectLease(GPUMemoryObjectLease),
    CudaHostMemAlloc(CudaHostMemAllocStru),
    RestoreDataReady,
    HibernateMem(HibernateMem),
    Fds(Fds),
}

pub const TSOTMSG_BUFF_SIZE: usize = core::mem::size_of::<TsotMsg>();

impl TsotMsg {
    pub fn AsBytes(&self) -> &'static [u8] {
        let addr = self as *const _ as u64 as *const u8;
        return unsafe { core::slice::from_raw_parts(addr, size_of::<Self>()) };
    }

    pub fn FromBytes(bytes: &[u8]) -> Self {
        assert!(bytes.len() == size_of::<Self>());
        let addr = &bytes[0] as *const _ as u64;
        let ret = unsafe { *(addr as *const Self) };
        return ret;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Fds {
    pub count: usize,
    pub offset: usize,
    pub total: usize,
}

impl Fds {
    pub fn Full(&self) -> bool {
        return self.total == self.offset + self.count;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HibernateMem {
    pub cnt: usize,
    pub idxs: [u16; 256],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PanicInfo {
    pub len: usize,
    pub str: [u8; 256],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CudaHostMemAllocStru {
    pub cnt: usize,
    pub idxs: [u16; 256],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GPUMemoryObjectLease {
    pub idx: usize,
    pub total: usize,
    pub processGPU: ProcessGPU,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SnapshotDataReady {
    pub gpuDatalen: [u64; 8],
    pub hostDataLen: u64,
    pub hibernateDataLen: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GPUMemoryOffloadReqDoneResp {
    pub gpuWorkerId: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GPUMemoryRestoreReqDoneResp {
    pub gpuWorkerId: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GPUMemoryFdsReqDoneResp {
    pub gpuWorkerId: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GPUMemoryOffloadReq {
    pub gpuWorkerId: u64,
    pub totalcnt: usize,
    pub reqMemLen: usize,
    pub reqMemAddr: u64,
    pub idx: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct SnapshotDone {
    upperdir: [u8; 512],
}

impl SnapshotDone {
    pub fn New(upperdir: &str) -> Self {
        let mut slice = [0; 512];
        let bytes = upperdir.as_bytes();
        assert!(bytes.len() <= slice.len());
        for i in 0..bytes.len() {
            slice[i] = bytes[i];
        }

        return Self { upperdir: slice };
    }

    pub fn Upperdir(&self) -> String {
        for i in 0..self.upperdir.len() {
            if self.upperdir[i] == 0 {
                return String::from_utf8(self.upperdir[0..i].to_vec()).unwrap();
            }
        }

        return String::from_utf8(self.upperdir[0..512].to_vec()).unwrap();
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GPUMemoryRestoreReq {
    pub gpuWorkerId: u64,
    pub totalcnt: usize,
    pub reqMemLen: usize,
    pub reqMemAddr: u64,
    pub idx: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GPUMemoryFdsReq {
    pub gpuWorkerId: u64,
}

// from pod to node agent
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PodRegisterReq {
    pub podUid: [u8; 16],
}

// from pod to node agent
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct GatewayRegisterReq {
    pub gatewayUid: [u8; 16],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CreateSocketReq {}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ListenReq {
    pub port: u16,
    pub backlog: u32,
}

// notify nodeagent one connected socket is consumed by user code
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AcceptReq {
    pub port: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StopListenReq {
    pub port: u16,
}

#[derive(Debug, Clone, Copy)]
pub enum ConnectReq {
    PodConnectReq(PodConnectReq),
    GatewayConnectReq(GatewayConnectReq),
}

// send with the socket fd
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PodConnectReq {
    pub reqId: u32,
    pub dstIp: u32,
    pub dstPort: u16,
    pub srcPort: u16,
}

// send with the socket fd
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GatewayConnectReq {
    pub reqId: u32,
    pub podNamespace: [u8; 64],
    pub dstIp: u32,
    pub dstPort: u16,
    pub srcPort: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DnsReq {
    pub reqId: u16,
    pub nameslen: u16,
    pub names: [u8; 256],
}

impl DnsReq {
    pub fn GetDomains(&self) -> Vec<String> {
        let namesStr =
            unsafe { String::from_utf8_unchecked(self.names[0..self.nameslen as usize].to_vec()) };

        let names: Vec<&str> = namesStr.split(":").collect();
        let mut domains = Vec::new();
        for name in names {
            domains.push(name.to_owned());
        }
        return domains;
    }
}

//////////////////////////////////////////////////////
// from nodeagent to pod

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PodRegisterResp {
    // the pod's container IP addr
    pub containerIp: u32,
    pub errorCode: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GatewayRegisterResp {
    pub errorCode: u32,
}

// send with new socket fd
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CreateSocketResp {}

#[derive(Debug, Clone, Copy)]
pub enum HibernateType {
    Mem,
    Ctx,
    All,
}

impl Default for HibernateType {
    fn default() -> Self {
        return Self::All;
    }
}

// send with new socket fd
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Hibernate {
    pub _type: HibernateType,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct HibernateDone {
    pub _type: HibernateType,
}

pub const MAX_GPU_COUNT: usize = 8;
pub const ONE_GB: u64 = 1 << 30;
pub const GPU_SLOT_SIZE: u64 = ONE_GB / 4;

pub const GPU_SLOT_SIZE_NAME: &'static str = "vRam_slot_size";
pub const GPU_SLOT_COUNT_NAME: &'static str = "vRam_slot_count";

#[derive(Debug, Serialize, Deserialize, Clone, Default, Copy)]
pub struct QGPUResourceInfo {
    pub total: u32,
    // phyGpuId --> SlotCnt
    pub map: [u32; MAX_GPU_COUNT],
    pub slotSize: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Wakeup {
    pub gpuMapping: QGPUResourceInfo,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct WakeupDone {
    pub gpuMapping: QGPUResourceInfo,
}

// another pod connected to current pod
// send with new socket fd
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PeerConnectNotify {
    pub peerIp: u32,
    pub peerPort: u16,
    pub localPort: u16,
}

impl PeerConnectNotify {
    pub fn PeerAddrBytes(&self) -> [u8; 4] {
        let addr = &self.peerIp as *const _ as u64 as *const u8;
        let bytes = unsafe { core::slice::from_raw_parts(addr, 4) };

        let mut ret: [u8; 4] = [0; 4];

        ret.copy_from_slice(bytes);
        return ret;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PodConnectResp {
    pub reqId: u32,
    pub errorCode: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GatewayConnectResp {
    pub reqId: u32,
    pub errorCode: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct DnsResp {
    pub reqId: u16,
    // maxinum 4 request and response
    pub ips: [u32; 4],
    pub count: usize,
}
