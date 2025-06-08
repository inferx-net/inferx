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

use core::result::Result as SResult;
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
// use serde_derive::Deserialize;
// use serde_derive::Serialize;

use crate::common::*;

pub const MAX_GPU_COUNT: usize = 8;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone, PartialOrd, Ord)]

pub struct GPUType(pub String);

impl Default for GPUType {
    fn default() -> Self {
        return Self::Any();
    }
}

impl GPUType {
    pub fn Any() -> Self {
        return Self("Any".to_string());
    }

    pub fn Unknown() -> Self {
        return Self("Unknown".to_string());
    }

    pub fn CanAlloc(&self, req: &Self) -> bool {
        if &req.0 == "Any" {
            return true;
        }

        return self == req;
    }
}

pub trait ResourceTrait {
    fn Equ(&self, other: &Self);
    fn GreaterOrEqu(&self, other: &Self) -> bool;
    fn Sub(&mut self, other: &Self) -> Result<()>;
    fn Add(&mut self, other: &Self) -> Result<()>;
}

// max gpu count per node
pub type GPUId = u8;

#[derive(Default, Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct NodeGPUResource {
    pub type_: GPUType,
    pub gpus: BTreeSet<GPUId>, // list of available GPUId
}

impl NodeGPUResource {
    pub fn New(gpuType: GPUType, gpus: &[GPUId]) -> Self {
        let mut set = BTreeSet::new();
        for id in gpus {
            set.insert(*id as GPUId);
        }

        return Self {
            type_: gpuType,
            gpus: set,
        };
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct GPUResource {
    #[serde(rename = "Type", default)]
    pub type_: GPUType,
    #[serde(rename = "Count")]
    pub gpuCount: u64,
    #[serde(rename = "vRam")]
    pub vRam: u64,
}

impl GPUResource {
    pub fn Sub(&mut self, other: &GPUResource) {
        if other.gpuCount == 0 || other.vRam == 0 {
            return;
        }
        assert!(self.gpuCount == other.gpuCount);
        self.vRam -= other.vRam;
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum GPUUage {
    vRam(u64),
    Full,
}

impl Default for GPUUage {
    fn default() -> Self {
        return Self::Full;
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone, Copy, PartialOrd, Ord)]
pub enum ResourceType {
    CPU, // 1/1000 CPU cores
    Mem, // MB memory
    GPU,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum GPUSet {
    Auto,
    GPUSet(BTreeSet<GPUId>),
}

impl Default for GPUSet {
    fn default() -> Self {
        return Self::Auto;
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ResourceConfig {
    #[serde(rename = "Mem", default)]
    pub allocMemory: u64, // MB memory
    #[serde(rename = "CacheMemory", default)]
    pub cacheMemory: u64,
    #[serde(rename = "Enable2MBPage", default)]
    pub enable2MBPage: bool,
    #[serde(rename = "EnableBlob", default)]
    pub enableBlob: bool,
    #[serde(rename = "BlobBuffer", default)]
    pub blobBuffer: u64,
    #[serde(rename = "GPUs", default)]
    pub gpus: GPUSet,

    #[serde(rename = "CPU", default)]
    pub cpu: u64, // 1/1000 CPU cores

    #[serde(rename = "ContextOverhead")]
    pub contextOverhead: u64, // MB vRam per GPU

    #[serde(rename = "MaxContextPerGPU")]
    pub maxContextPerGPU: u64, // max
}

impl ResourceConfig {
    pub fn MaxContextPerGPU(&self) -> u64 {
        if self.maxContextPerGPU == 0 {
            return 1;
        }

        return self.maxContextPerGPU;
    }

    pub fn AllocableMem(&self) -> u64 {
        let mut memory = self.allocMemory - self.cacheMemory;
        if self.enableBlob {
            memory -= self.blobBuffer;
        }

        return memory;
    }
}

impl GPUResourceMap {
    pub fn Gpus(&self) -> Vec<i32> {
        let gpus = self.map.keys().cloned().collect();
        return gpus;
    }

    pub fn Add(&mut self, alloc: &Self) {
        if self.slotSize == 0 {
            self.slotSize = alloc.slotSize;
        }
        // assert!(
        //     self.slotSize == alloc.slotSize,
        //     "self.slotSize is {} alloc {}",
        //     self.slotSize,
        //     alloc.slotSize
        // );
        for (pGpuId, resource) in &alloc.map {
            match self.map.get_mut(pGpuId) {
                None => {
                    self.map.insert(*pGpuId, resource.clone());
                }
                Some(alloc) => {
                    alloc.slotCnt += resource.slotCnt;
                    alloc.contextCnt += resource.contextCnt;
                    alloc.ncclCnt += resource.ncclCnt;
                }
            }
        }
    }

    pub fn Sub(&mut self, alloc: &Self) {
        assert!(
            alloc.slotSize == 0 || self.slotSize == alloc.slotSize,
            "self.slotSize is {} alloc.slotSize is {}",
            self.slotSize,
            alloc.slotSize
        );

        for (pGpuId, resource) in &alloc.map {
            match self.map.get_mut(pGpuId) {
                None => unreachable!(),
                Some(cnt) => {
                    cnt.slotCnt -= resource.slotCnt;
                    cnt.contextCnt -= resource.contextCnt;
                    cnt.ncclCnt -= resource.ncclCnt;
                }
            }
        }
    }

    pub fn ReqSlotCnt(&self, vRam: u64) -> u32 {
        let vRam = vRam * 1024 * 1024; // MB to Bytes
        let slotCnt = (vRam + self.slotSize - 1) / self.slotSize;
        return slotCnt as u32;
    }

    pub fn Alloc(&mut self, usage: &GPUResource) -> Result<Self> {
        let gpus = self.CanAlloc(usage);
        if gpus.is_none() {
            return Err(Error::SchedulerNoEnoughResource(format!("")));
        }

        let gpus = gpus.unwrap();

        let mut map = BTreeMap::new();

        let mut count = usage.gpuCount;
        if count == 0 {
            return Ok(Self {
                totalSlotCnt: self.totalSlotCnt,
                map: map,
                slotSize: self.slotSize,
            });
        }

        let ncclCnt = if count == 1 { 0 } else { 1 };

        let slotCnt = self.ReqSlotCnt(usage.vRam);

        for gpu in gpus {
            match self.map.get_mut(&gpu) {
                None => unreachable!(),
                Some(resource) => {
                    resource.contextCnt -= 1;
                    resource.slotCnt -= slotCnt;
                    resource.ncclCnt -= ncclCnt;
                }
            }

            map.insert(
                gpu,
                GPUAlloc {
                    contextCnt: 1,
                    slotCnt: slotCnt,
                    ncclCnt: ncclCnt,
                },
            );
            count -= 1;

            if count == 0 {
                break;
            }
        }

        return Ok(Self {
            totalSlotCnt: self.totalSlotCnt,
            map: map,
            slotSize: self.slotSize,
        });
    }

    pub fn CanAlloc(&self, usage: &GPUResource) -> Option<Vec<i32>> {
        let mut cnt = usage.gpuCount;
        if cnt == 0 {
            return Some(Vec::new());
        }

        let mut gpus = Vec::new();

        let ncclCnt = if cnt < 2 { 0 } else { 1 };

        let reqSlotCnt = self.ReqSlotCnt(usage.vRam);
        for (&pGpuId, resource) in &self.map {
            if resource.contextCnt == 0 {
                continue;
            }

            if resource.ncclCnt < ncclCnt {
                continue;
            }

            if resource.slotCnt >= reqSlotCnt {
                cnt -= 1;
                gpus.push(pGpuId);
                if cnt == 0 {
                    return Some(gpus);
                }
            }
        }

        // error!(
        //     "can't alloc GPU reqSlotCnt {} map {:?}",
        //     reqSlotCnt, &self.map
        // );

        return None;
    }

    // 0: SlotCnt 1: phyGpuId
    fn GenArr(&self) -> Vec<(u32, i32)> {
        let mut v = Vec::with_capacity(self.map.len());
        for (phyGpuId, resource) in &self.map {
            v.push((resource.slotCnt, *phyGpuId));
        }

        return v;
    }
}

#[derive(Debug, Clone, Default)]
pub struct GPUAllocation {
    pub gpuId: GPUId,
    pub usage: u8,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct NodeResources {
    pub nodename: String,
    #[serde(rename = "CPU", default)]
    pub cpu: u64, // 1/1000 CPU cores
    #[serde(rename = "Mem", default)]
    pub memory: u64, // MB memory
    #[serde(rename = "CacheMem", default)]
    pub cacheMemory: u64,
    #[serde(rename = "GPUType", default)]
    pub gpuType: GPUType,
    #[serde(rename = "GPUs", default)]
    pub gpus: GPUResourceMap,
    #[serde(rename = "MaxContextPerGPU", default)]
    pub maxContextCnt: u64,
}

impl NodeResources {
    pub fn New(
        nodename: &str,
        cpu: u64,
        memory: u64,
        cacheMemory: u64,
        gpuType: GPUType,
        gpus: GPUResourceMap,
        maxContextPerGpu: u64,
    ) -> Self {
        return Self {
            nodename: nodename.to_owned(),
            cpu: cpu,
            memory: memory,
            cacheMemory: cacheMemory,
            gpuType: gpuType.clone(),
            gpus: gpus,
            maxContextCnt: maxContextPerGpu,
        };
    }

    pub fn Copy(&self) -> Self {
        return Self {
            nodename: self.nodename.clone(),
            cpu: self.cpu,
            memory: self.memory,
            cacheMemory: self.cacheMemory,
            gpuType: self.gpuType.clone(),
            gpus: self.gpus.clone(),
            maxContextCnt: self.maxContextCnt,
        };
    }

    pub fn GPUResource(&self) -> Self {
        return Self {
            nodename: "".to_owned(),
            cpu: 0,
            memory: 0,
            cacheMemory: 0,
            gpuType: self.gpuType.clone(),
            gpus: self.gpus.clone(),
            maxContextCnt: self.maxContextCnt,
        };
    }

    pub fn CanAlloc(&self, req: &Resources) -> bool {
        let canAlloc = self.cpu >= req.cpu
            && self.memory >= req.memory
            && self.cacheMemory >= req.cacheMemory
            && self.gpuType.CanAlloc(&req.gpu.type_)
            && self.gpus.CanAlloc(&req.gpu).is_some();

        if false && !canAlloc {
            let _cpu = self.cpu >= req.cpu;
            let _memory = self.memory >= req.memory;
            let _cacheMemory = self.cacheMemory >= req.cacheMemory;
            let _gpuType = self.gpuType.CanAlloc(&req.gpu.type_);
            let _gpus = self.gpus.CanAlloc(&req.gpu).is_some();

            if !_memory {
                error!(
                    "self.memory is {} required memory is {}",
                    self.memory, req.memory
                );
            }

            error!("CanAlloc fail cpu:{_cpu} memory:{_memory}, cacheMemory:{_cacheMemory}, gpuType:{_gpuType}, gpus:{_gpus}");
        }

        return canAlloc;
    }

    pub fn Sub(&mut self, other: &Self) -> Result<()> {
        // error!("NodeResources sub \n curr is {:?} \n sub {:?}", self, other);
        // self.cpu -= other.cpu;
        self.memory -= other.memory;
        self.cacheMemory -= other.cacheMemory;
        self.gpus.Sub(&other.gpus);

        return Ok(());
    }

    // use for restore a container, the container will set the cgroup with required resource but not allocated
    pub fn ResourceQuota(&self, resource: &Resources) -> Self {
        return Self {
            nodename: self.nodename.clone(),
            cpu: resource.cpu,
            memory: resource.memory,
            cacheMemory: resource.cacheMemory,
            gpuType: self.gpuType.clone(),
            gpus: GPUResourceMap::default(),
            maxContextCnt: self.maxContextCnt,
        };
    }

    pub fn Alloc(&mut self, req: &Resources) -> Result<NodeResources> {
        if !self.CanAlloc(req) {
            error!(
                "NodeResources::alloc fail available {:#?} require {:#?}",
                self, req
            );
            return Err(Error::SchedulerNoEnoughResource(format!(
                "NodeResources::alloc fail available {:?} require {:?}",
                self, req
            )));
        }

        // we don't allc/free cpu resource, assume there are enough cpu resource
        // self.cpu -= req.cpu;
        self.memory -= req.memory;
        self.cacheMemory -= req.cacheMemory;
        let gpus = self.gpus.Alloc(&req.gpu)?;

        return Ok(NodeResources {
            nodename: self.nodename.clone(),
            cpu: req.cpu,
            memory: req.memory,
            cacheMemory: req.cacheMemory,
            gpuType: self.gpuType.clone(),
            gpus: gpus,
            maxContextCnt: self.maxContextCnt,
        });
    }

    pub fn Add(&mut self, free: &NodeResources) -> Result<()> {
        assert!(self.gpuType == free.gpuType);
        self.gpus.Add(&free.gpus);
        // self.cpu += free.cpu;
        self.memory += free.memory;
        self.cacheMemory += free.cacheMemory;

        return Ok(());
    }

    pub fn Gpus(&self) -> GPUResourceMap {
        return self.gpus.clone();
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Resources {
    #[serde(rename = "CPU")]
    pub cpu: u64, // 1/1000 CPU cores
    #[serde(rename = "Mem")]
    pub memory: u64, // MB memory
    #[serde(default, rename = "CacheMem")]
    pub cacheMemory: u64,
    #[serde(rename = "GPU")]
    pub gpu: GPUResource,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            cpu: 0,
            memory: 0,
            cacheMemory: 0,
            gpu: GPUResource::default(),
        }
    }
}

impl Resources {
    pub fn SetDefault(&mut self) {
        if self.cpu == 0 {
            self.cpu = 100; // default 0.1 cpu
        }

        if self.memory == 0 {
            self.memory = 500; // default 500 MB
        }
    }

    pub fn GPUResource(&self) -> Self {
        return Self {
            cpu: 0,
            memory: 0,
            cacheMemory: 0,
            gpu: self.gpu.clone(),
        };
    }

    pub fn Sub(&mut self, other: &Self) {
        self.cpu -= other.cpu;
        self.memory -= other.memory;
        self.cacheMemory -= other.cacheMemory;
        self.gpu.Sub(&other.gpu);
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NodeResourcesStatus {
    pub total: NodeResources,
    pub available: NodeResources,
}

impl NodeResourcesStatus {
    // Returns true if the node has the available resources to run the task.
    pub fn IsAvailable(&self, req: &Resources) -> bool {
        return self.available.CanAlloc(req);
    }

    // Returns true if the node's total resources are enough to run the task.
    pub fn IsFeasible(&self, req: &Resources) -> bool {
        return self.total.CanAlloc(req);
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct GPUResourceMap {
    // total slotCnt
    pub totalSlotCnt: u32,
    // phyGpuId --> GPUResource
    pub map: BTreeMap<i32, GPUAlloc>,
    pub slotSize: u64,
}

impl Serialize for GPUResourceMap {
    fn serialize<S>(&self, serializer: S) -> SResult<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let allocRam = match self.map.first_key_value() {
            None => 0,
            Some((_, alloc)) => alloc.slotCnt as u64 * self.slotSize / 1024 / 1024,
        };

        let mut s = serializer.serialize_struct("GPUResourceMap", 3)?;
        s.serialize_field("vRam", &allocRam)?;
        s.serialize_field("map", &self.map)?;
        s.serialize_field("slotSize", &self.slotSize)?;
        s.serialize_field("totalSlotCnt", &self.totalSlotCnt)?;
        s.end()
    }
}

impl GPUResourceMap {
    pub fn VRam(&self, gpuId: i32) -> u64 {
        return self.SlotCnt(gpuId) as u64 * self.slotSize;
    }

    pub fn FirstVRam(&self) -> u64 {
        return self.FirstSlotCnt() as u64 * self.slotSize;
    }

    pub fn FirstSlotCnt(&self) -> u32 {
        match self.map.first_key_value() {
            None => return 0,
            Some((_, resource)) => {
                let slotCnt = resource.slotCnt;
                return slotCnt;
            }
        }
    }

    pub fn TotalVRam(&self) -> u64 {
        return self.totalSlotCnt as u64 * self.slotSize;
    }

    pub fn SlotCnt(&self, gpuId: i32) -> u32 {
        match self.map.get(&gpuId) {
            None => return 0,
            Some(resource) => return resource.slotCnt,
        }
    }

    pub fn GPUResourceInfo(&self) -> GPUResourceInfo {
        let mut info = GPUResourceInfo {
            total: self.totalSlotCnt,
            map: [0; MAX_GPU_COUNT],
            slotSize: self.slotSize,
        };

        for (pGpuId, resource) in &self.map {
            info.map[*pGpuId as usize] = resource.slotCnt;
        }

        return info;
    }

    pub fn VirtToPhy(&self) -> BTreeMap<i32, i32> {
        let mut map = BTreeMap::new();

        let mut vGpu = 0;
        for pGpu in self.map.keys() {
            map.insert(vGpu, pGpu.clone());
            vGpu += 1;
        }

        return map;
    }

    pub fn PhyToVirt(&self) -> BTreeMap<i32, i32> {
        let mut map = BTreeMap::new();
        let mut vGpu = 0;
        for pGpu in self.map.keys() {
            map.insert(pGpu.clone(), vGpu);
            vGpu += 1;
        }
        return map;
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default, Copy)]
pub struct GPUResourceInfo {
    pub total: u32,
    // phyGpuId --> SlotCnt
    pub map: [u32; MAX_GPU_COUNT],
    pub slotSize: u64,
}

impl GPUResourceInfo {
    pub fn GPUResourceMap(&self) -> GPUResourceMap {
        let mut map = GPUResourceMap {
            totalSlotCnt: self.total,
            map: BTreeMap::new(),
            slotSize: self.slotSize,
        };

        let mut gpuCnt = 0;

        for i in 0..self.map.len() {
            if self.map[i] > 0 {
                gpuCnt += 1;
            }
        }

        let ncclCnt = if gpuCnt > 1 { 1 } else { 0 };

        for i in 0..self.map.len() {
            if self.map[i] > 0 {
                let gpuResource = GPUAlloc {
                    contextCnt: 1,
                    slotCnt: self.map[i],
                    ncclCnt: ncclCnt,
                };
                map.map.insert(i as i32, gpuResource);
            }
        }

        return map;
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct GPUAlloc {
    pub contextCnt: u64,
    pub slotCnt: u32,
    pub ncclCnt: u64,
}

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
    pub fn GpuMemKeepalive(&self, blobStoreEnable: bool) -> StandbyType {
        return self.gpuMem.StandbyType(blobStoreEnable);
    }

    pub fn Needblob(&self) -> bool {
        return self.gpuMem == StandbyType::Blob
            || self.pageableMem == StandbyType::Blob
            || self.pinndMem == StandbyType::Blob;
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
    pub fn StandbyType(&self, blobStoreEnable: bool) -> StandbyType {
        let keepalive = match self {
            StandbyType::Mem => StandbyType::Mem,
            StandbyType::File => StandbyType::File,
            StandbyType::Blob => {
                if blobStoreEnable {
                    StandbyType::Blob
                } else {
                    StandbyType::File
                }
            }
        };

        return keepalive;
    }

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
