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

use core::ops::Deref;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

use crate::metastore::informer::{EventHandler, Informer};
use crate::metastore::selection_predicate::ListOption;
use crate::metastore::store::ThreadSafeStore;
use crate::peer_mgr::NA_CONFIG;
use inferxlib::data_obj::{DeltaEvent, EventType};
use inferxlib::obj_mgr::node_mgr::Node;
use inferxlib::obj_mgr::node_mgr::NodeSpec;

use crate::common::*;
use crate::metastore::aggregate_client::AggregateClient;
use crate::metastore::cache_store::{CacheStore, ChannelRev};

#[derive(Debug)]
pub struct IxAggrStoreInner {
    pub agents: BTreeMap<String, IxAgent>,
    pub podStore: CacheStore,
    pub nodeStore: CacheStore,
    pub snapshotStore: CacheStore,

    pub listNotify: Arc<Notify>,
    pub listDone: bool,
    pub notifies: Vec<Arc<Notify>>,
}

#[derive(Debug, Clone)]
pub struct IxAggrStore(Arc<Mutex<IxAggrStoreInner>>);

impl Deref for IxAggrStore {
    type Target = Arc<Mutex<IxAggrStoreInner>>;

    fn deref(&self) -> &Arc<Mutex<IxAggrStoreInner>> {
        &self.0
    }
}

impl IxAggrStore {
    pub async fn New(channelRev: &ChannelRev) -> Result<Self> {
        let inner = IxAggrStoreInner {
            agents: BTreeMap::new(),
            podStore: CacheStore::New(None, "pod", 0, channelRev).await?,
            nodeStore: CacheStore::New(None, "node", 0, channelRev).await?,
            snapshotStore: CacheStore::New(None, "snapshot", 0, channelRev).await?,
            listDone: false,
            listNotify: Arc::new(Notify::new()),
            notifies: Vec::new(),
        };

        return Ok(Self(Arc::new(Mutex::new(inner))));
    }

    pub fn PodStore(&self) -> CacheStore {
        return self.lock().unwrap().podStore.clone();
    }

    pub fn NodeStore(&self) -> CacheStore {
        return self.lock().unwrap().nodeStore.clone();
    }

    pub fn SnapshotStore(&self) -> CacheStore {
        return self.lock().unwrap().snapshotStore.clone();
    }

    pub async fn Process(&self) -> Result<()> {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let informer = Informer::New(
            NA_CONFIG.stateSvcAddrs.clone(),
            Node::KEY,
            "",
            "",
            &ListOption::default(),
        )?;

        informer.AddEventHandler(Arc::new(self.clone())).await?;
        let notify = Arc::new(Notify::new());
        informer.Process(notify).await?;
        return Ok(());
    }

    pub fn AddIxAgent(&self, nodeName: &str, svcAddr: &str) -> Result<Vec<Arc<Notify>>> {
        let podStore = self.PodStore();
        let nodeStore = self.NodeStore();
        let snapshotStore = self.SnapshotStore();

        let agent = IxAgent::New(nodeName, svcAddr, &nodeStore, &podStore, &snapshotStore)?;

        self.lock()
            .unwrap()
            .agents
            .insert(nodeName.to_owned(), agent.clone());

        let mut notifies = Vec::new();
        let nodeListNotify = Arc::new(Notify::new());
        let podListNotify = Arc::new(Notify::new());
        let snapshotListNotify = Arc::new(Notify::new());

        notifies.push(nodeListNotify.clone());
        notifies.push(podListNotify.clone());
        notifies.push(snapshotListNotify.clone());

        tokio::spawn(async move {
            agent
                .Process(nodeListNotify, podListNotify, snapshotListNotify)
                .await
                .unwrap();
        });

        return Ok(notifies);
    }

    pub fn RemoveIxAgent(&self, nodeName: &str) -> Result<()> {
        let agent = match self.lock().unwrap().agents.remove(nodeName) {
            Some(a) => a,
            None => {
                return Err(Error::NotExist(format!(
                    "IxAggrStore::RemoveIxAgent {}",
                    nodeName
                )));
            }
        };

        agent.Close();
        return Ok(());
    }

    pub async fn WaitlistDone(&self) {
        let listdone = self.lock().unwrap().listDone;
        if listdone {
            return;
        }

        let listNotify = self.lock().unwrap().listNotify.clone();

        listNotify.notified().await;

        let mut notifies = Vec::new();
        notifies.append(&mut self.lock().unwrap().notifies);
        let mut futures = Vec::new();
        for n in &notifies {
            futures.push(n.notified());
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                // wait max 500 ms
            }
            _ = futures::future::join_all(futures) => {
                self.lock().unwrap().listNotify.notify_waiters();
            }
        }

        assert!(self.lock().unwrap().listDone == false); // we don't allow multiple waiter
        self.lock().unwrap().listDone = true;
    }
}

use async_trait::async_trait;
#[async_trait]
impl EventHandler for IxAggrStore {
    async fn handle(&self, _store: &ThreadSafeStore, event: &DeltaEvent) {
        let listdone = self.lock().unwrap().listDone;
        if listdone {
            match &event.type_ {
                EventType::Added => {
                    let obj = &event.obj;
                    let node = obj.To::<NodeSpec>().unwrap();

                    let stateSvcPort: u16 = node.object.stateSvcPort;
                    let stateSvcAddr = format!("http://{}:{}", &node.object.nodeIp, stateSvcPort);
                    error!("IxAggrStore::handle [AFTER_INIT] Added node: name={} ip={} port={} revision={} channelRev={}",
                        &node.name, &node.object.nodeIp, stateSvcPort, obj.revision, obj.channelRev);
                    self.AddIxAgent(&node.name, &stateSvcAddr).unwrap();
                }
                EventType::Deleted => {
                    let obj = event.oldObj.as_ref().unwrap();
                    let node = obj.To::<NodeSpec>().unwrap();
                    error!("IxAggrStore::handle [AFTER_INIT] Deleted node: name={} ip={} revision={} channelRev={}",
                        &node.name, &node.object.nodeIp, obj.revision, obj.channelRev);
                    self.RemoveIxAgent(&node.name).unwrap();
                }
                _ => {
                    error!("IxAggrStore::handle get unexpect event {:#?}", event);
                }            }
        } else {
            match &event.type_ {
                EventType::Added => {
                    let obj = &event.obj;
                    let nodeInfo: Node = obj.To::<NodeSpec>().expect(&format!(
                        "NodeMgr::handle deserialize fail for {}",
                        &obj.object
                    ));

                    let stateSvcPort: u16 = nodeInfo.object.stateSvcPort;
                    let stateSvcAddr =
                        format!("http://{}:{}", &nodeInfo.object.nodeIp, stateSvcPort);

                    error!("IxAggrStore::handle [DURING_INIT] Added node: name={} ip={} port={} revision={} channelRev={} inInitialList={}",
                        &nodeInfo.name, &nodeInfo.object.nodeIp, stateSvcPort, obj.revision, obj.channelRev, event.inInitialList);
                    let mut notifies = self.AddIxAgent(&nodeInfo.name, &stateSvcAddr).unwrap();
                    self.lock().unwrap().notifies.append(&mut notifies);
                }
                EventType::Deleted => {
                    let obj = event.oldObj.as_ref().unwrap();

                    let nodeInfo: Node = obj.To::<NodeSpec>().expect(&format!(
                        "NodeMgr::handle deserialize fail for {}",
                        &obj.object
                    ));

                    error!("IxAggrStore::handle [DURING_INIT] Deleted node: name={} ip={} revision={} channelRev={} inInitialList={}",
                        &nodeInfo.name, &nodeInfo.object.nodeIp, obj.revision, obj.channelRev, event.inInitialList);
                    self.RemoveIxAgent(&nodeInfo.name).unwrap();
                }
                EventType::InitDone => {
                    error!("IxAggrStore::handle [INIT_DONE] InitList completed, starting watch");
                    self.lock().unwrap().listNotify.notify_waiters();
                }
                _ => {
                    error!("IxAggrStore::handle get unexpect event {:#?}", event);
                },
            }
        }
    }
}

#[derive(Debug)]
pub struct IxAgentInner {
    pub closeNotify: Arc<Notify>,
    pub closed: AtomicBool,

    pub nodeName: String,
    pub svcAddr: String,
    pub nodeClient: AggregateClient,
    pub podClient: AggregateClient,
    pub snapshotClient: AggregateClient,
}

#[derive(Debug, Clone)]
pub struct IxAgent(Arc<IxAgentInner>);

impl Deref for IxAgent {
    type Target = Arc<IxAgentInner>;

    fn deref(&self) -> &Arc<IxAgentInner> {
        &self.0
    }
}

impl IxAgent {
    pub fn New(
        nodeName: &str,
        svcAddr: &str,
        nodeCache: &CacheStore,
        podCache: &CacheStore,
        snapshotCache: &CacheStore,
    ) -> Result<Self> {
        let inner = IxAgentInner {
            closeNotify: Arc::new(Notify::new()),
            closed: AtomicBool::new(false),

            nodeName: nodeName.to_owned(),
            svcAddr: svcAddr.to_owned(),
            nodeClient: AggregateClient::New(nodeCache, "node", "", "")?,
            podClient: AggregateClient::New(podCache, "pod", "", "")?,
            snapshotClient: AggregateClient::New(snapshotCache, "snapshot", "", "")?,
        };

        return Ok(Self(Arc::new(inner)));
    }

    pub fn Close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.closeNotify.notify_waiters();
    }

    pub async fn Process(
        &self,
        nodeListNotify: Arc<Notify>,
        podListNotify: Arc<Notify>,
        snapshotListNotify: Arc<Notify>,
    ) -> Result<()> {
        let clone = self.clone();
        tokio::spawn(async move {
            clone
                .snapshotClient
                .Process(vec![clone.svcAddr.clone()], snapshotListNotify.clone())
                .await
                .unwrap();
        });

        let clone = self.clone();
        tokio::spawn(async move {
            clone
                .podClient
                .Process(vec![clone.svcAddr.clone()], podListNotify.clone())
                .await
                .unwrap();
        });

        let clone = self.clone();

        // slow down node sync to make pod and snapshot ready before it to avoid create more snapshot pod
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        tokio::spawn(async move {
            clone
                .nodeClient
                .Process(vec![clone.svcAddr.clone()], nodeListNotify.clone())
                .await
                .unwrap();
        });

        self.closeNotify.notified().await;
        self.nodeClient.Close();
        self.podClient.Close();
        self.snapshotClient.Close();
        return Ok(());
    }
}
