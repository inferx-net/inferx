use serde::{Deserialize, Serialize};

use crate::resource::NodeResources;

use crate::data_obj::*;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct NodeSpec {
    pub nodeIp: String,
    pub podMgrPort: u16,
    pub tsotSvcPort: u16,
    pub stateSvcPort: u16,
    pub cidr: String,
    pub resources: NodeResources,
    pub blobStoreEnable: bool,
}

pub type Node = DataObject<NodeSpec>;
pub type NodeMgr = DataObjectMgr<NodeSpec>;

impl Node {
    pub const KEY: &'static str = "node_info";
    pub const TENANT: &'static str = "system";
    pub const NAMESPACE: &'static str = "system";

    pub fn NodeAgentUrl(&self) -> String {
        return format!("http://{}:{}", self.object.nodeIp, self.object.podMgrPort);
    }
}
