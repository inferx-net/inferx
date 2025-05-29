use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::common::*;
use crate::data_obj::{DataObject, DataObjectMgr};
use crate::resource::{GPUResource, Resources, Standby, StandbyType};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SnapshotInfo {
    pub hostMemSize: u64,
    pub processCheckpointSize: u64,
    pub fatbinSize: u64,
    pub gpuMemSizes: BTreeMap<i32, u64>,
    pub standby: Standby,
}

pub const ONE_GB: u64 = 1 << 30;

impl SnapshotInfo {
    pub fn SnapshotStandyInfo(&self) -> SnapshotStandyInfo {
        let mut gpu = 0;
        for (_, size) in &self.gpuMemSizes {
            gpu += *size;
        }
        return SnapshotStandyInfo {
            pageable: self.processCheckpointSize,
            gpu: gpu,
            pinned: self.hostMemSize,
        };
    }

    pub fn StandyCacheMemory(&self) -> u64 {
        let mut cacheMemory = 0;
        if self.standby.gpuMem == StandbyType::Mem {
            for (_, size) in &self.gpuMemSizes {
                cacheMemory += (*size + ONE_GB - 1) / ONE_GB * 1024;
            }
        }

        if self.standby.pinndMem == StandbyType::Mem {
            cacheMemory += (self.hostMemSize + ONE_GB - 1) / ONE_GB * 1024;
        }

        return cacheMemory;
    }

    pub fn ReadyCacheMemory(&self) -> u64 {
        // the cudahostmemory will be keep in cache memory when in ready state
        let cacheMemory = (self.hostMemSize + ONE_GB - 1) / ONE_GB * 1024;
        return cacheMemory;
    }

    pub fn StandbyMemory(&self) -> u64 {
        let mut memory = 0;

        if self.standby.pageableMem == StandbyType::Mem {
            memory += (self.processCheckpointSize + ONE_GB - 1) / ONE_GB * 1024; 
        }

        return memory;
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SnapshotStandyInfo {
    pub pageable: u64,
    pub pinned: u64,
    pub gpu: u64,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Default, Clone)]
pub struct SnapshotMeta {
    pub imagename: String,
    pub buildId: Vec<u8>,
}

impl SnapshotMeta {
    pub fn Load(root: &str) -> Result<Self> {
        let path = format!("{}/{}", root, "meta.data");
        let str: String = std::fs::read_to_string(path)?;
        let u = match serde_json::from_str(&str) {
            Ok(u) => u,
            Err(e) => {
                return Err(Error::CommonError(format!(
                    "SnapshotMeta load fail for {} with error {:?}",
                    root, e
                )));
            }
        };
        return Ok(u);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ContainerSnapshot {
    pub funckey: String,
    pub nodename: String,
    pub state: SnapshotState,
    pub meta: SnapshotMeta,
    pub info: SnapshotInfo,
}

impl ContainerSnapshot {
    pub fn StandbyResource(&self) -> Resources {
        return Resources {
            cpu: 0,
            memory: self.info.StandbyMemory(),
            cacheMemory: self.info.StandyCacheMemory(),
            gpu: GPUResource::default(),
        };
    }

    pub fn ReadyCacheMemory(&self) -> u64 {
        return self.info.ReadyCacheMemory();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Copy, PartialEq, Eq)]
pub enum SnapshotState {
    Loading,
    Ready,
}

impl Default for SnapshotState {
    fn default() -> Self {
        return Self::Loading;
    }
}

impl ContainerSnapshot {
    pub const KEY: &'static str = "snapshot";
}

pub type FuncSnapshot = DataObject<ContainerSnapshot>;
pub type FuncSnapshotMgr = DataObjectMgr<ContainerSnapshot>;
