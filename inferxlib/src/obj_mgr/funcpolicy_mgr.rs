use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::data_obj::*;
use crate::resource;
use crate::resource::*;

pub const DEFAULT_PARALLEL_LEVEL: usize = 2;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FuncPolicySpec {
    #[serde(rename = "min_replica")]
    pub minReplica: u64,
    #[serde(rename = "max_replica")]
    pub maxReplica: u64,
    #[serde(rename = "standby_per_node")]
    pub standbyPerNode: u64,
    #[serde(default = "default_parallel", rename = "parallel")]
    pub parallel: usize,
}

fn default_parallel() -> usize {
    DEFAULT_PARALLEL_LEVEL
}

impl Default for FuncPolicySpec {
    fn default() -> Self {
        return Self {
            minReplica: 0,
            maxReplica: 10,
            standbyPerNode: 1,
            parallel: DEFAULT_PARALLEL_LEVEL,
        };
    }
}

pub type FuncPolicy = DataObject<FuncPolicySpec>;
pub type FuncPolicyMgr = DataObjectMgr<FuncPolicySpec>;

impl FuncPolicy {
    pub const KEY: &'static str = "funcpolicy";
}
