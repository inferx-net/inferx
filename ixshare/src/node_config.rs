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

use serde::{Deserialize, Serialize};
use std::fs;
use std::net::IpAddr;
use std::sync::Mutex;

use crate::common::*;
use crate::consts::*;
use crate::gateway::auth_layer::KeycloadConfig;
use inferxlib::resource::{GPUSet, ResourceConfig};

use std::collections::BTreeSet;
use std::num::ParseIntError;

pub const SNAPSHOT_DIR: &str = "/opt/inferx/snapshot";

lazy_static::lazy_static! {
    #[derive(Debug)]
    pub static ref NODE_CONFIG: NodeConfig = {
        let args : Vec<String> = std::env::args().collect();
        info!("NODE_CONFIG args is {:?}", &args);
        let configfilePath = if args.len() == 1 {
            "/opt/inferx/config/node.json"
        } else {
            &args[1]
        };

        let config = NodeConfig::Load(configfilePath).expect(&format!("can't load config from {}", configfilePath));
        config
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuSelection {
    All,
    Indexes(Vec<usize>),
}

#[derive(Debug)]
pub enum ParseGpuError {
    Empty,
    InvalidSegment(String),
    InvalidNumber(ParseIntError),
    InvalidRange(String),
}

impl From<ParseIntError> for ParseGpuError {
    fn from(e: ParseIntError) -> Self {
        ParseGpuError::InvalidNumber(e)
    }
}

/// Parse docker `--gpus`-style strings into a `GpuSelection`.
///
/// Examples:
/// - "all" -> GpuSelection::All
/// - "1" -> Indexes([1])
/// - "1-3" -> Indexes([1,2,3])
/// - "0,2,4-6" -> Indexes([0,2,4,5,6])
pub fn ParseGpuString(s: &str) -> Result<GPUSet> {
    let s_trim = s.trim();
    if s_trim.is_empty() {
        return Err(Error::CommonError(format!("ParseGpuString fail {}", s)));
    }

    if s_trim.eq_ignore_ascii_case("all") {
        return Ok(GPUSet::Auto);
    }

    let mut indices = BTreeSet::new();

    for part in s_trim.split(',') {
        let p = part.trim();
        if p.is_empty() {
            return Err(Error::CommonError(format!("ParseGpuString fail {}", s)));
        }

        // Range case: "m-n"
        if let Some(idx) = p.find('-') {
            let start_str = &p[..idx].trim();
            let end_str = &p[idx + 1..].trim();
            if start_str.is_empty() || end_str.is_empty() {
                return Err(Error::CommonError(format!("ParseGpuString fail {}", s)));
            }
            let start: usize = start_str.parse()?;
            let end: usize = end_str.parse()?;
            if start > end {
                return Err(Error::CommonError(format!("ParseGpuString fail {}", s)));
            }
            for v in start..=end {
                indices.insert(v as u8);
            }
        } else {
            // Single index
            let v: usize = p.parse()?;
            indices.insert(v as u8);
        }
    }

    return Ok(GPUSet::GPUSet(indices));
}

pub fn GetLocalIp(hostIpCidr: &str) -> Option<String> {
    use std::str::FromStr;
    let hostIpCidr = ipnetwork::Ipv4Network::from_str(hostIpCidr).unwrap();
    let hostIpCidrMask = !((1 << hostIpCidr.prefix()) - 1);
    let hostIpCidrIp = IpAddress::New(&hostIpCidr.ip().octets());

    match std::env::var("POD_IP") {
        Err(_) => (),
        Ok(s) => {
            info!("get local ip from env POD_IP: {}", &s);
            return Some(s);
        }
    };

    let network_interfaces = match local_ip_address::list_afinet_netifas() {
        Err(e) => {
            error!("fail to get GetLocalIp {:?}", &e);
            panic!();
        }
        Ok(i) => i,
    };
    for (_name, ip) in network_interfaces.iter() {
        match ip {
            IpAddr::V4(ipv4) => {
                let localIp = IpAddress::New(&ipv4.octets());
                if localIp.0 & hostIpCidrMask == hostIpCidrIp.0 {
                    let bytes = ipv4.octets();
                    return Some(format!(
                        "{}.{}.{}.{}",
                        bytes[0], bytes[1], bytes[2], bytes[3]
                    ));
                }
            }
            _ => (),
        }
    }

    return None;
}

#[derive(Debug)]
pub struct GatewayConfig {
    pub nodeName: String,
    pub etcdAddrs: Vec<String>,
    pub stateSvcAddrs: Vec<String>,
    pub nodeIp: String,
    pub schedulerPort: u16,
    pub auditdbAddr: String,
    pub enforceBilling: bool,
    pub secretStoreAddr: String,
    pub keycloakconfig: KeycloadConfig,
    pub inferxAdminApikey: String,
    pub gatewayPort: u16,
}

impl GatewayConfig {
    pub fn New(config: &NodeConfig) -> Self {
        assert!(config.etcdAddrs.len() > 0);
        assert!(config.stateSvcAddrs.len() > 0);

        let nodeName = match std::env::var("NODE_NAME") {
            Ok(s) => {
                info!("get nodename from env NODE_NAME: {}", &s);
                s
            }
            Err(_) => {
                if config.nodeName.len() == 0 {
                    gethostname::gethostname()
                        .to_str()
                        .unwrap_or("")
                        .to_string()
                } else {
                    config.nodeName.clone()
                }
            }
        };

        let etcdAddrs = match std::env::var("ETCD_ADDR") {
            Ok(s) => vec![s],
            Err(_) => config.etcdAddrs.clone(),
        };

        let nodeIp = if config.nodeIp.len() == 0 {
            assert!(config.hostIpCidr.len() != 0);
            let nodeIp = GetLocalIp(&config.hostIpCidr).unwrap();
            nodeIp
        } else {
            config.nodeIp.clone()
        };

        let schedulerPort = if config.schedulerPort == 0 {
            DEFAULT_SCHEDULER_PORT
        } else {
            config.schedulerPort
        };

        let stateSvcAddrs = match std::env::var("STATESVC_ADDR") {
            Ok(s) => vec![s],
            Err(_) => config.stateSvcAddrs.clone(),
        };

        let secretStoreAddr = match std::env::var("SECRDB_ADDR") {
            Ok(s) => s,
            Err(_) => config.secretStoreAddr.clone(),
        };

        let auditdbAddr = match std::env::var("AUDITDB_ADDR") {
            Ok(s) => s,
            Err(_) => config.auditdbAddr.clone(),
        };

        let enforceBilling = match std::env::var("ENFORCE_BILLING") {
            Ok(s) => match s.parse::<bool>() {
                Ok(v) => v,
                Err(_) => {
                    warn!("invalid ENFORCE_BILLING value '{}', defaulting to false", &s);
                    false
                }
            },
            Err(_) => false,
        };

        let keycloakUrl = match std::env::var("KEYCLOAK_URL") {
            Ok(s) => s,
            Err(_) => config.keycloakconfig.url.clone(),
        };

        let keycloakRealm = match std::env::var("KEYCLOAK_REALM") {
            Ok(s) => s,
            Err(_) => config.keycloakconfig.realm.clone(),
        };

        let inferxAdminApikey = match std::env::var("INFERX_ADMIN_APIKEY") {
            Ok(s) => {
                info!("get inferxAdminApikey from env INFERX_ADMIN_APIKEY: {}", &s);
                s
            }
            Err(_) => String::new(),
        };

        let gatewayPort = if config.gatewayPort == 0 {
            DEFAULT_GATEWAY_PORT
        } else {
            config.gatewayPort
        };

        let ret = Self {
            nodeName: nodeName,
            etcdAddrs: etcdAddrs,
            stateSvcAddrs: stateSvcAddrs,
            nodeIp: nodeIp,
            schedulerPort: schedulerPort,
            secretStoreAddr: secretStoreAddr,
            auditdbAddr: auditdbAddr,
            enforceBilling: enforceBilling,
            keycloakconfig: KeycloadConfig {
                url: keycloakUrl,
                realm: keycloakRealm,
            },
            inferxAdminApikey: inferxAdminApikey,
            gatewayPort: gatewayPort,
        };

        info!("GatewayConfig is {:#?}", &ret);

        return ret;
    }
}

#[derive(Debug)]
pub struct SchedulerConfig {
    pub etcdAddrs: Vec<String>,
    pub stateSvcAddrs: Vec<String>,
    pub nodeIp: String,
    pub schedulerPort: u16,
    pub auditdbAddr: String,
}

impl SchedulerConfig {
    pub fn New(config: &NodeConfig) -> Self {
        assert!(config.etcdAddrs.len() > 0);
        assert!(config.stateSvcAddrs.len() > 0);

        let etcdAddrs = match std::env::var("ETCD_ADDR") {
            Ok(s) => vec![s],
            Err(_) => config.etcdAddrs.clone(),
        };

        let nodeIp = if config.nodeIp.len() == 0 {
            assert!(config.hostIpCidr.len() != 0);
            let nodeIp = GetLocalIp(&config.hostIpCidr).unwrap();
            nodeIp
        } else {
            config.nodeIp.clone()
        };

        let schedulerPort = if config.schedulerPort == 0 {
            DEFAULT_SCHEDULER_PORT
        } else {
            config.schedulerPort
        };

        let stateSvcAddrs = match std::env::var("STATESVC_ADDR") {
            Ok(s) => vec![s],
            Err(_) => config.stateSvcAddrs.clone(),
        };

        let auditdbAddr = match std::env::var("AUDITDB_ADDR") {
            Ok(s) => s,
            Err(_) => config.auditdbAddr.clone(),
        };

        let ret = Self {
            etcdAddrs: etcdAddrs,
            stateSvcAddrs: stateSvcAddrs,
            nodeIp: nodeIp,
            schedulerPort: schedulerPort,
            auditdbAddr: auditdbAddr,
        };

        info!("SchedulerConfig is {:#?}", &ret);

        return ret;
    }
}

#[derive(Debug)]
pub struct StateSvcConfig {
    pub etcdAddrs: Vec<String>,
    pub svcIp: String,
    pub stateSvcPort: u16,
    pub auditdbAddr: String,
}

impl StateSvcConfig {
    pub fn New(config: &NodeConfig) -> Self {
        let etcdAddrs = match std::env::var("ETCD_ADDR") {
            Ok(s) => vec![s],
            Err(_) => config.etcdAddrs.clone(),
        };

        let svcIp = if config.nodeIp.len() == 0 {
            assert!(config.hostIpCidr.len() != 0);
            let nodeIp = GetLocalIp(&config.hostIpCidr).unwrap();
            nodeIp
        } else {
            config.nodeIp.clone()
        };

        let stateSvcPort = if config.stateSvcPort == 0 {
            DEFAULT_STATESVC_PORT
        } else {
            config.stateSvcPort
        };

        let auditdbAddr = match std::env::var("AUDITDB_ADDR") {
            Ok(s) => s,
            Err(_) => config.auditdbAddr.clone(),
        };

        let ret = Self {
            etcdAddrs,
            svcIp: svcIp,
            stateSvcPort: stateSvcPort,
            auditdbAddr: auditdbAddr,
        };

        info!("StateSvcConfig is {:#?}", &ret);

        return ret;
    }
}

fn ParseK8sMemory(mem_str: &str) -> Option<u64> {
    let units = [
        ("Ki", 1024u64),
        ("Mi", 1024u64.pow(2)),
        ("Gi", 1024u64.pow(3)),
        ("Ti", 1024u64.pow(4)),
        ("Pi", 1024u64.pow(5)),
        ("Ei", 1024u64.pow(6)),
    ];

    for (unit, multiplier) in units.iter() {
        if mem_str.ends_with(unit) {
            let num_str = mem_str.trim_end_matches(unit);
            if let Ok(num) = num_str.parse::<f64>() {
                return Some((num * (*multiplier as f64)) as u64);
            } else {
                return None;
            }
        }
    }

    // If it's just a number (assuming bytes)
    if let Ok(num) = mem_str.parse::<u64>() {
        return Some(num);
    }

    None
}

#[derive(Debug)]
pub struct NodeAgentConfig {
    pub etcdAddrs: Vec<String>,
    pub stateSvcAddrs: Vec<String>,

    pub nodeName: String,
    pub nodeIp: String,

    pub podMgrPort: u16,
    pub tsotCniPort: u16,
    pub tsotSvcPort: u16,
    pub stateSvcPort: u16,

    pub cidr: Mutex<String>,
    pub tsotSocketPath: String,
    pub tsotGwSocketPath: String,
    pub resources: ResourceConfig,
    pub snapshotDir: String,
    pub enableBlobStore: bool,
    pub memcache: ShareMem,
    pub tlsconfig: TLSConfig,
    pub auditdbAddr: String,
    pub secretStoreAddr: String,

    pub initCudaHostAllocSize: i64, // GB
}

impl NodeAgentConfig {
    pub fn New(config: &NodeConfig) -> Self {
        let nodeIp = if config.nodeIp.len() == 0 {
            assert!(config.hostIpCidr.len() != 0);
            let nodeIp = match GetLocalIp(&config.hostIpCidr) {
                None => {
                    panic!("can't get local ip");
                }
                Some(i) => i,
            };
            nodeIp
        } else {
            config.nodeIp.clone()
        };

        let nodeName = match std::env::var("NODE_NAME") {
            Ok(s) => {
                info!("get nodename from env NODE_NAME: {}", &s);
                s
            }
            Err(_) => {
                if config.nodeName.len() == 0 {
                    gethostname::gethostname()
                        .to_str()
                        .unwrap_or("")
                        .to_string()
                } else {
                    config.nodeName.clone()
                }
            }
        };

        let podMgrPort = if config.podMgrPort == 0 {
            DEFAULT_PODMGR_PORT
        } else {
            config.podMgrPort
        };

        let tsotCniPort = if config.tsotCniPort == 0 {
            DEFAULT_TSOTCNI_PORT
        } else {
            config.tsotCniPort
        };

        let tsotSvcPort = if config.tsotSvcPort == 0 {
            DEFAULT_TSOTSVC_PORT
        } else {
            config.tsotSvcPort
        };

        let nodeagentStateSvcPort = if config.nodeagentStateSvcPort == 0 {
            DEFAULT_NASTATESVC_PORT
        } else {
            config.nodeagentStateSvcPort
        };

        let tsotSocketPath = if config.tsotSocketPath.len() == 0 {
            TSOT_SOCKET_PATH.to_string()
        } else {
            config.tsotSocketPath.clone()
        };

        let tsotGwSocketPath = if config.tsotGwSocketPath.len() == 0 {
            TSOT_GW_SOCKET_PATH.to_string()
        } else {
            config.tsotGwSocketPath.clone()
        };

        let snapshotDir = config.SnapshotDir();

        assert!(config.cidr.len() != 0);
        assert!(config.etcdAddrs.len() > 0);
        assert!(config.stateSvcAddrs.len() > 0);

        let mut resources = config.resources.clone();

        resources.allocMemory = match std::env::var("ALLOC_MEMORY") {
            Ok(s) => {
                info!("get memory from env ALLOC_MEMORY: {}", &s);
                let size = ParseK8sMemory(&s);
                match size {
                    None => resources.allocMemory,
                    Some(s) => s / 1024u64.pow(2),
                }
            }
            Err(_) => resources.allocMemory,
        };

        let initCudaHostAllocSize = match std::env::var("INIT_CUDAHOSTALLOC") {
            Ok(s) => {
                info!(
                    "get initCudaHostAllocSize from env INIT_CUDAHOSTALLOC: {}",
                    &s
                );
                let size = s.parse::<i64>();
                match size {
                    Err(_) => {
                        info!("fail to parse initCudaHostAllocSize from env INIT_CUDAHOSTALLOC: {}, use default value -1 (disabled)", &s);
                        -1
                    }
                    Ok(s) => {
                        if s < 0 {
                            info!("get initCudaHostAllocSize from env INIT_CUDAHOSTALLOC: {}, disable cudahostAlloc", &s);
                        }
                        s
                    }
                }
            }
            Err(_) => -1, // disable by default
        };

        resources.cacheMemory = match std::env::var("CACHE_MEMORY") {
            Ok(s) => {
                info!("get cacheMemory from env CACHE_MEMORY: {}", &s);
                let size = ParseK8sMemory(&s);
                match size {
                    None => {
                        error!("invalid CACHE_MEMORY environment variable {}", &s);
                        panic!("invalid CACHE_MEMORY environment variable {}", &s);
                    }
                    Some(s) => {
                        let res = s / 1024u64.pow(2);
                        if res > resources.allocMemory {
                            error!(
                                "CACHE_MEMORY {} MB can't exceed ALLOC_MEMORY {} MB",
                                res, resources.allocMemory
                            );
                            panic!(
                                "CACHE_MEMORY {} MB can't exceed ALLOC_MEMORY {} MB",
                                res, resources.allocMemory
                            );
                        }
                        res
                    }
                }
            }
            Err(_) => {
                warn!("missing CACHE_MEMORY environment variable");
                20 * 1024
            }
        };

        resources.enable2MBPage = match std::env::var("ENABLE_2MB_PAGE") {
            Ok(s) => {
                info!("get enable2MBPage from env ENABLE_2MB_PAGE: {}", &s);
                let enable2MBPage = s.parse::<bool>();
                match enable2MBPage {
                    Err(_) => {
                        error!("invalid ENABLE_2MB_PAGE environment variable {}", &s);
                        panic!("invalid ENABLE_2MB_PAGE environment variable {}", &s);
                    }
                    Ok(s) => s,
                }
            }
            Err(_) => false,
        };

        resources.enableBlob = match std::env::var("ENALBE_BLOB") {
            Ok(s) => {
                info!("get enableBlob from env ENALBE_BLOB: {}", &s);
                let enable2MBPage = s.parse::<bool>();
                match enable2MBPage {
                    Err(_) => {
                        error!("invalid ENALBE_BLOB environment variable {}", &s);
                        panic!("invalid ENALBE_BLOB environment variable {}", &s);
                    }
                    Ok(b) => {
                        if b {
                            if !resources.enable2MBPage {
                                error!("Invalid ENALBE_BLOB, Blob store require ENABLE_2MB_PAGE is true");
                                panic!("Invalid ENALBE_BLOB, Blob store require ENABLE_2MB_PAGE is true");
                            }
                        }
                        b
                    }
                }
            }
            Err(_) => false,
        };

        resources.blobBuffer = match std::env::var("BLOB_BUFF") {
            Ok(s) => {
                info!("get blobBuffer from env BLOB_BUFF: {}", &s);
                let size = ParseK8sMemory(&s);
                match size {
                    None => {
                        error!("invalid BLOB_BUFF environment variable {}", &s);
                        panic!("invalid BLOB_BUFF environment variable {}", &s);
                    }
                    Some(s) => {
                        let res = s / 1024u64.pow(2);
                        res
                    }
                }
            }
            Err(_) => 4 * 1024, // 4GB
        };

        resources.cpu = match std::env::var("ALLOC_CPU") {
            Ok(s) => {
                info!("get cpu from env ALLOC_CPU: {}", &s);
                match s.parse::<u64>() {
                    Err(_) => resources.cpu,
                    Ok(c) => c * 1000,
                }
            }
            Err(_) => resources.cpu,
        };

        resources.gpus = match std::env::var("CUDA_VISIBLE_DEVICES") {
            Ok(s) => {
                info!("get GPU map from env CUDA_VISIBLE_DEVICES: {}", &s);
                let gpuset = match ParseGpuString(&s) {
                    Ok(s) => s,
                    Err(_e) => {
                        error!(
                            "fail to pass CUDA_VISIBLE_DEVICES {:?} fallback to all gpu",
                            s
                        );
                        GPUSet::Auto
                    }
                };
                gpuset
            }
            Err(_) => GPUSet::Auto,
        };

        resources.maxContextPerGPU = match std::env::var("CONTEXT_COUNT") {
            Ok(s) => {
                info!("get context_count from env CONTEXT_COUNT: {}", &s);
                match s.parse::<u64>() {
                    Err(_) => resources.maxContextPerGPU,
                    Ok(c) => c,
                }
            }
            Err(_) => resources.maxContextPerGPU,
        };

        resources.reserveMemPercentage = match std::env::var("RESERVE_MEM_PERCENTAGE") {
            Ok(s) => {
                info!(
                    "get reserveMemPercentage from env RESERVE_MEM_PERCENTAGE: {}",
                    &s
                );
                match s.parse::<u64>() {
                    Err(_) => 0,
                    Ok(c) => c,
                }
            }
            Err(_) => 0,
        };

        resources.contextOverhead = match std::env::var("CONTEXT_OVERHEAD") {
            Ok(s) => {
                info!("get context_overhead from env CONTEXT_OVERHEAD: {}", &s);
                match s.parse::<u64>() {
                    Err(_) => resources.contextOverhead,
                    Ok(c) => c,
                }
            }
            Err(_) => resources.contextOverhead,
        };

        let stateSvcAddrs = match std::env::var("STATESVC_ADDR") {
            Ok(s) => vec![s],
            Err(_) => config.stateSvcAddrs.clone(),
        };

        let etcdAddrs = match std::env::var("ETCD_ADDR") {
            Ok(s) => vec![s],
            Err(_) => config.etcdAddrs.clone(),
        };

        let secretStoreAddr = match std::env::var("SECRDB_ADDR") {
            Ok(s) => s,
            Err(_) => config.secretStoreAddr.clone(),
        };

        let auditdbAddr = match std::env::var("AUDITDB_ADDR") {
            Ok(s) => s,
            Err(_) => config.auditdbAddr.clone(),
        };

        let ret = Self {
            etcdAddrs: etcdAddrs,
            stateSvcAddrs: stateSvcAddrs,
            nodeName: nodeName,
            nodeIp: nodeIp,

            podMgrPort: podMgrPort,
            tsotCniPort: tsotCniPort,
            tsotSvcPort: tsotSvcPort,
            stateSvcPort: nodeagentStateSvcPort,

            cidr: Mutex::new(String::new()),
            tsotSocketPath: tsotSocketPath,
            tsotGwSocketPath: tsotGwSocketPath,
            resources: resources, // config.resources.clone(),
            snapshotDir: snapshotDir,
            enableBlobStore: config.enableBlobStore,
            memcache: config.sharemem.clone(),
            tlsconfig: config.tlsconfig.clone(),
            auditdbAddr: auditdbAddr,
            secretStoreAddr: secretStoreAddr,
            initCudaHostAllocSize: initCudaHostAllocSize,
        };

        info!("Nodeagent Config  {:#?}", &ret);

        return ret;
    }

    pub fn SetCidr(&self, cidr: &str) {
        *self.cidr.lock().unwrap() = cidr.to_owned();
    }

    pub fn Cidr(&self) -> String {
        return self.cidr.lock().unwrap().clone();
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ShareMem {
    pub size: u64, // GB
    pub hugepage: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct TLSConfig {
    pub enable: bool,
    #[serde(default, rename = "cert")]
    pub certpath: String,
    #[serde(default, rename = "key")]
    pub keypath: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct NodeConfig {
    #[serde(default)]
    pub nodeName: String,
    pub etcdAddrs: Vec<String>,

    #[serde(default)]
    pub nodeIp: String,
    #[serde(default)]
    pub hostIpCidr: String,

    #[serde(default)]
    pub podMgrPort: u16,
    #[serde(default)]
    pub tsotCniPort: u16,
    #[serde(default)]
    pub tsotSvcPort: u16,
    #[serde(default)]
    pub nodeagentStateSvcPort: u16,
    #[serde(default)]
    pub stateSvcPort: u16,
    #[serde(default)]
    pub schedulerPort: u16,
    #[serde(default)]
    pub gatewayPort: u16,

    #[serde(default)]
    pub cidr: String,
    #[serde(default)]
    pub stateSvcAddrs: Vec<String>,

    #[serde(default)]
    pub tsotSocketPath: String,
    #[serde(default)]
    pub tsotGwSocketPath: String,

    pub runService: bool,

    #[serde(default)]
    pub auditdbAddr: String,

    #[serde(default)]
    pub resources: ResourceConfig,

    #[serde(default)]
    pub snapshotDir: String,

    #[serde(default)]
    pub enableBlobStore: bool,

    #[serde(default)]
    pub sharemem: ShareMem,

    #[serde(default)]
    pub tlsconfig: TLSConfig,

    #[serde(default)]
    pub keycloakconfig: KeycloadConfig,

    #[serde(default)]
    pub secretStoreAddr: String,

    #[serde(default)]
    pub peerLoad: bool,
}

impl NodeConfig {
    pub fn Load(path: &str) -> Result<Self> {
        let data = fs::read_to_string(path)?;
        let config: NodeConfig = serde_json::from_str(&data)?;
        return Ok(config);
    }

    pub fn SnapshotDir(&self) -> String {
        if self.snapshotDir.len() == 0 {
            return SNAPSHOT_DIR.to_owned();
        } else {
            return self.snapshotDir.clone();
        }
    }
}
