use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::data_obj::*;
use crate::resource;
use crate::resource::*;

pub const DEFAULT_PARALLEL_LEVEL: usize = 50;
pub const DEFAULT_QUEUE_LEN: usize = 100;
pub const DEFAULT_QUEUE_TIMEOUT: f64 = 30.0;
pub const DEFAULT_SCALEIN_TIMEOUT: f64 = 1.0; // 1000 ms
pub const DEFAULT_QUEUE_RATIO: f64 = 0.1;
pub const DEFAULT_PLATFORM_ENDPOINT_MAX_REPLICA: u64 = 4;
pub const VIRTUAL_ENDPOINTS_NAMESPACE: &str = "endpoints";

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
    #[serde(default = "default_nvidia_replica", rename = "nvidia_replica")]
    pub nvidiaReplica: u64,

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

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PartialFuncPolicySpec {
    #[serde(default, rename = "min_replica")]
    pub minReplica: Option<u64>,
    #[serde(default, rename = "max_replica")]
    pub maxReplica: Option<u64>,
    #[serde(default, rename = "standby_per_node")]
    pub standbyPerNode: Option<u64>,
    #[serde(default, rename = "parallel")]
    pub parallel: Option<usize>,
    #[serde(default, rename = "queue_len")]
    pub queueLen: Option<usize>,
    #[serde(default, rename = "queue_timeout")]
    pub queueTimeout: Option<f64>,
    #[serde(default, rename = "scaleout_policy")]
    pub scaleoutPolicy: Option<ScaleOutPolicy>,
    #[serde(default, rename = "scalein_timeout")]
    pub scaleinTimeout: Option<f64>,
    #[serde(default, rename = "runtime_config")]
    pub runtimeConfig: Option<RuntimeConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct FuncPolicySchedulerDefaults {
    #[serde(default, rename = "min_replica")]
    pub minReplica: Option<u64>,
    #[serde(default, rename = "max_replica")]
    pub maxReplica: Option<u64>,
    #[serde(default, rename = "standby_per_node")]
    pub standbyPerNode: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EndpointGatewayPolicySpec {
    #[serde(default = "default_max_replica", rename = "max_replica")]
    pub maxReplica: u64,
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

fn default_nvidia_replica() -> u64 {
    0
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

fn default_max_replica() -> u64 {
    1
}

impl Default for FuncPolicySpec {
    fn default() -> Self {
        return Self {
            nvidiaReplica: 0,
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

impl PartialFuncPolicySpec {
    pub fn Normalize(&self, namespace: &str) -> FuncPolicySpec {
        let mut policy = FuncPolicySpec::default();

        if namespace == VIRTUAL_ENDPOINTS_NAMESPACE {
            policy.standbyPerNode = 0;
        }

        if let Some(v) = self.minReplica {
            policy.minReplica = v;
        }
        if let Some(v) = self.maxReplica {
            policy.maxReplica = v;
        }
        if let Some(v) = self.standbyPerNode {
            policy.standbyPerNode = v;
        }
        if let Some(v) = self.parallel {
            policy.parallel = v;
        }
        if let Some(v) = self.queueLen {
            policy.queueLen = v;
        }
        if let Some(v) = self.queueTimeout {
            policy.queueTimeout = v;
        }
        if let Some(v) = &self.scaleoutPolicy {
            policy.scaleoutPolicy = v.clone();
        }
        if let Some(v) = self.scaleinTimeout {
            policy.scaleinTimeout = v;
        }
        if let Some(v) = &self.runtimeConfig {
            policy.runtimeConfig = v.clone();
        }

        policy
    }
}

impl FuncPolicySchedulerDefaults {
    pub fn ApplyTo(&self, policy: &mut FuncPolicySpec) {
        if let Some(v) = self.minReplica {
            policy.minReplica = v;
        }
        if let Some(v) = self.maxReplica {
            policy.maxReplica = v;
        }
        if let Some(v) = self.standbyPerNode {
            policy.standbyPerNode = v;
        }
    }
}

impl EndpointGatewayPolicySpec {
    pub fn AsFuncPolicySpec(&self) -> FuncPolicySpec {
        FuncPolicySpec {
            nvidiaReplica: 0,
            minReplica: 0,
            maxReplica: self.maxReplica,
            standbyPerNode: 0,
            parallel: self.parallel,
            queueLen: self.queueLen,
            queueTimeout: self.queueTimeout,
            scaleoutPolicy: self.scaleoutPolicy.clone(),
            scaleinTimeout: self.scaleinTimeout,
            runtimeConfig: default_runtime_config(),
        }
    }
}

impl Default for EndpointGatewayPolicySpec {
    fn default() -> Self {
        Self {
            maxReplica: default_max_replica(),
            parallel: default_parallel(),
            queueLen: default_queue_len(),
            queueTimeout: default_queue_timeout(),
            scaleoutPolicy: default_ScaleOutPolicy(),
            scaleinTimeout: default_scalein_timeout(),
        }
    }
}

pub fn NormalizeFuncPolicyDataObject(
    obj: &DataObject<Value>,
) -> crate::common::Result<DataObject<Value>> {
    let partial: DataObject<PartialFuncPolicySpec> = obj.To::<PartialFuncPolicySpec>()?;
    let normalized = DataObject {
        objType: partial.objType,
        tenant: partial.tenant,
        namespace: partial.namespace.clone(),
        name: partial.name,
        labels: partial.labels,
        annotations: partial.annotations,
        channelRev: partial.channelRev,
        srcEpoch: partial.srcEpoch,
        revision: partial.revision,
        object: partial.object.Normalize(&partial.namespace),
    };

    Ok(normalized.DataObject())
}

pub type FuncPolicy = DataObject<FuncPolicySpec>;
pub type FuncPolicyMgr = DataObjectMgr<FuncPolicySpec>;

impl FuncPolicy {
    pub const KEY: &'static str = "funcpolicy";
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_policy_normalization_uses_endpoint_defaults_for_virtual_endpoints() {
        let partial = PartialFuncPolicySpec {
            queueLen: Some(42),
            ..Default::default()
        };

        let normalized = partial.Normalize(VIRTUAL_ENDPOINTS_NAMESPACE);
        assert_eq!(normalized.minReplica, 0);
        assert_eq!(normalized.maxReplica, 1);
        assert_eq!(normalized.standbyPerNode, 0);
        assert_eq!(normalized.queueLen, 42);
    }

    #[test]
    fn scheduler_defaults_overlay_preserves_unset_fields() {
        let mut base = FuncPolicySpec {
            minReplica: 2,
            maxReplica: 1,
            standbyPerNode: 3,
            ..Default::default()
        };

        FuncPolicySchedulerDefaults {
            minReplica: Some(0),
            maxReplica: Some(DEFAULT_PLATFORM_ENDPOINT_MAX_REPLICA),
            standbyPerNode: None,
        }
        .ApplyTo(&mut base);

        assert_eq!(base.minReplica, 0);
        assert_eq!(base.maxReplica, DEFAULT_PLATFORM_ENDPOINT_MAX_REPLICA);
        assert_eq!(base.standbyPerNode, 3);
    }

    #[test]
    fn normalize_funcpolicy_data_object_materializes_full_policy() {
        let raw = DataObject {
            objType: FuncPolicy::KEY.to_owned(),
            tenant: "t1".to_owned(),
            namespace: VIRTUAL_ENDPOINTS_NAMESPACE.to_owned(),
            name: "f1".to_owned(),
            labels: Default::default(),
            annotations: Default::default(),
            channelRev: 0,
            srcEpoch: 0,
            revision: 0,
            object: json!({
                "max_replica": 3,
                "parallel": 99
            }),
        };

        let normalized = NormalizeFuncPolicyDataObject(&raw).unwrap();
        let materialized = FuncPolicy::FromDataObject(normalized).unwrap();

        assert_eq!(materialized.object.minReplica, 0);
        assert_eq!(materialized.object.maxReplica, 3);
        assert_eq!(materialized.object.standbyPerNode, 0);
        assert_eq!(materialized.object.parallel, 99);
    }
}
