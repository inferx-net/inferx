use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::data_obj::*;
use crate::resource;
use crate::resource::*;

pub const DEFAULT_PARALLEL_LEVEL: usize = 50;
pub const DEFAULT_QUEUE_LEN: usize = 100;
pub const DEFAULT_QUEUE_TIMEOUT: f64 = 30.0;
pub const DEFAULT_SCALEIN_TIMEOUT: f64 = 1.0; // 1000 ms
pub const DEFAULT_QUEUE_RATIO: f64 = 0.1;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ScaleOutPolicy {
    WaitQueueRatio(WaitQueueRatio),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WaitQueueRatio {
    #[serde(rename = "wait_ratio")]
    pub waitRatio: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RuntimeConfig {
    #[serde(default = "default_graph_sync", rename = "graph_sync")]
    pub GraphSync: bool,
}

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

    #[serde(default = "default_queue_len", rename = "queue_len")]
    pub queueLen: usize,

    #[serde(default = "default_queue_timeout", rename = "queue_timeout")]
    pub queueTimeout: f64,

    #[serde(default = "default_ScaleOutPolicy", rename = "scaleout_policy")]
    pub scaleoutPolicy: ScaleOutPolicy,

    #[serde(default = "default_scalein_timeout", rename = "scalein_timeout")]
    pub scaleinTimeout: f64,

    #[serde(default = "default_runtime_config", rename = "runtime_config")]
    pub runtimeConfig: RuntimeConfig,
}

fn default_ScaleOutPolicy() -> ScaleOutPolicy {
    ScaleOutPolicy::WaitQueueRatio(WaitQueueRatio {
        waitRatio: DEFAULT_QUEUE_RATIO,
    })
}

fn default_graph_sync() -> bool {
    false
}

fn default_runtime_config() -> RuntimeConfig {
    RuntimeConfig { GraphSync: false }
}

fn default_parallel() -> usize {
    DEFAULT_PARALLEL_LEVEL
}

fn default_queue_len() -> usize {
    DEFAULT_QUEUE_LEN
}

fn default_queue_timeout() -> f64 {
    DEFAULT_QUEUE_TIMEOUT
}

fn default_scalein_timeout() -> f64 {
    DEFAULT_SCALEIN_TIMEOUT
}

impl Default for FuncPolicySpec {
    fn default() -> Self {
        return Self {
            minReplica: 0,
            maxReplica: 1,
            standbyPerNode: 1,
            parallel: DEFAULT_PARALLEL_LEVEL,
            queueLen: default_queue_len(),
            queueTimeout: default_queue_timeout(),
            scaleoutPolicy: default_ScaleOutPolicy(),
            scaleinTimeout: default_scalein_timeout(),
            runtimeConfig: default_runtime_config(),
        };
    }
}

pub type FuncPolicy = DataObject<FuncPolicySpec>;
pub type FuncPolicyMgr = DataObjectMgr<FuncPolicySpec>;

impl FuncPolicy {
    pub const KEY: &'static str = "funcpolicy";
}
