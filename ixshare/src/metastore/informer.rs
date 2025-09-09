// Copyright (c) 2025 InferX Authors / 2014 The Kubernetes Authors
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
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::Duration;
use std::{collections::BTreeMap, fmt::Debug, sync::Arc};

use tokio::sync::Notify;

use rand::Rng;

use super::cacher_client::*;
use super::selection_predicate::*;
use super::store::ThreadSafeStore;
use crate::common::*;
use inferxlib::data_obj::*;

pub trait EventHandler: Debug + Send + Sync {
    fn handle(&self, store: &ThreadSafeStore, event: &DeltaEvent);
}

#[derive(Debug, Clone)]
pub struct Informer(Arc<InformerInner>);

impl Deref for Informer {
    type Target = Arc<InformerInner>;

    fn deref(&self) -> &Arc<InformerInner> {
        &self.0
    }
}

unsafe impl Send for Informer {}

impl Informer {
    pub fn New(
        addresses: Vec<String>,
        objType: &str,
        tenant: &str,
        namespace: &str,
        opts: &ListOption,
    ) -> Result<Self> {
        let inner = InformerInner {
            objType: objType.to_owned(),
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            opts: opts.DeepCopy(),
            revision: AtomicI64::new(0),
            store: ThreadSafeStore::default(),
            lastEventHandlerId: AtomicU64::new(0),
            serverAddresses: addresses,
            handlers: RwLock::new(BTreeMap::new()),
            closeNotify: Arc::new(Notify::new()),
            closed: AtomicBool::new(false),
            listDone: AtomicBool::new(false),
        };

        let informer = Self(Arc::new(inner));

        return Ok(informer);
    }

    pub fn Close(&self) -> Result<()> {
        let notify = self.closeNotify.clone();

        notify.notify_waiters();
        return Ok(());
    }

    pub fn AddEventHandler(&self, h: Arc<dyn EventHandler>) -> Result<u64> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::CommonError("the informer is closed".to_owned()));
        }

        let id = self.lastEventHandlerId.fetch_add(1, Ordering::SeqCst);

        let objs = self.store.List();
        for obj in objs {
            let event = DeltaEvent {
                type_: EventType::Added,
                inInitialList: true,
                obj: obj,
                oldObj: None,
            };

            h.handle(&self.store, &event);
        }

        self.handlers.write().unwrap().insert(id, h.clone());

        return Ok(id);
    }

    pub fn RemoveEventHandler(&mut self, id: u64) -> Option<Arc<dyn EventHandler>> {
        return self.handlers.write().unwrap().remove(&id);
    }

    pub async fn GetClient(&self) -> Option<CacherClient> {
        let size = self.serverAddresses.len();
        let offset: usize = rand::thread_rng().gen_range(0..size);
        loop {
            for i in 0..size {
                let idx = (offset + i) % size;
                let addr = &self.serverAddresses[idx];

                tokio::select! {
                    out = CacherClient::New(addr.clone()) => {
                        match out {
                            Ok(client) => return Some(client),
                            Err(e) => {
                                error!("informer::GetClient fail to connect to {} with error {:?}", addr, e);
                            }
                        }
                    }
                    _ = self.closeNotify.notified() => {
                        self.closed.store(true, Ordering::SeqCst);
                        return None
                    }
                };
            }

            // retry after one second
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(1000)) => {}
                _ = self.closeNotify.notified() => {
                    self.closed.store(true, Ordering::SeqCst);
                    return None
                }
            }
        }
    }

    async fn InitList(&self, client: &CacherClient) -> Result<()> {
        let store = self.store.clone();

        let objType = self.objType.clone();
        let tenant = self.tenant.clone();
        let namespace = self.namespace.clone();
        let opts = self.opts.DeepCopy();

        let objs = client.List(&objType, &tenant, &namespace, &opts).await?;
        self.revision.store(objs.revision, Ordering::SeqCst);
        // if &objType == "node_info" {
        //     error!("InitList objType {} {:#?}", &objType, &objs);
        // }

        for o in objs.objs {
            store.Add(&o)?;
            self.Distribute(&DeltaEvent {
                type_: EventType::Added,
                inInitialList: true,
                obj: o,
                oldObj: None,
            })
            .await;
        }

        let o = DataObject {
            objType: objType.clone(),
            ..Default::default()
        };

        self.Distribute(&DeltaEvent {
            type_: EventType::InitDone,
            inInitialList: true,
            obj: o,
            oldObj: None,
        })
        .await;

        return Ok(());
    }

    async fn WatchUpdate(&self, client: &CacherClient) -> Result<()> {
        let objType = self.objType.clone();
        let tenant = self.tenant.clone();
        let namespace = self.namespace.clone();
        let mut opts = self.opts.DeepCopy();
        opts.revision = self.revision.load(Ordering::Acquire);
        let store = self.store.clone();
        let closeNotify = self.closeNotify.clone();

        loop {
            let mut ws = client.Watch(&objType, &tenant, &namespace, &opts).await?;
            loop {
                let event = tokio::select! {
                    e = ws.Next() => {
                        e
                    }
                    _ = closeNotify.notified() => {
                        self.closed.store(true, Ordering::SeqCst);
                        return Ok(())
                    }
                };

                let event = match event {
                    Err(e) => {
                        error!(
                            "WatchUpdate type is {}/{} watch get error {:?}",
                            self.objType,
                            self.revision.load(Ordering::Acquire),
                            e
                        );
                        break;
                    }
                    Ok(e) => match e {
                        None => break,
                        Some(e) => {
                            opts.revision = e.obj.Revision();
                            self.revision.store(opts.revision, Ordering::SeqCst);
                            let de = match e.type_ {
                                EventType::Added => {
                                    store.Add(&e.obj)?;
                                    DeltaEvent {
                                        type_: e.type_,
                                        inInitialList: false,
                                        obj: e.obj.clone(),
                                        oldObj: None,
                                    }
                                }
                                EventType::Modified => {
                                    let oldObj = store.Update(&e.obj)?;
                                    DeltaEvent {
                                        type_: e.type_,
                                        inInitialList: false,
                                        obj: e.obj.clone(),
                                        oldObj: Some(oldObj),
                                    }
                                }
                                EventType::Deleted => {
                                    let old = store.Delete(&e.obj)?;
                                    DeltaEvent {
                                        type_: e.type_,
                                        inInitialList: false,
                                        obj: e.obj.clone(),
                                        oldObj: Some(old),
                                    }
                                }
                                _ => panic!("Informer::Process get unexpect type {:?}", e.type_),
                            };
                            de
                        }
                    },
                };

                self.Distribute(&event).await;
            }

            let objs = client.List(&objType, &tenant, &namespace, &opts).await?;
            opts.revision = objs.revision + 1;
            for o in objs.objs {
                self.Distribute(&DeltaEvent {
                    type_: EventType::Added,
                    inInitialList: false,
                    obj: o,
                    oldObj: None,
                })
                .await;
            }
        }
    }

    pub async fn Process(&self, notify: Arc<Notify>) -> Result<()> {
        let mut client = match self.GetClient().await {
            None => return Ok(()),
            Some(c) => c,
        };

        loop {
            match self.InitList(&client).await {
                Err(e) => {
                    error!("informer initlist fail with error {:?}", e);
                }
                Ok(()) => break,
            }

            client = match self.GetClient().await {
                None => return Ok(()),
                Some(c) => c,
            };
        }

        self.listDone.store(true, Ordering::SeqCst);
        notify.notify_waiters();

        loop {
            match self.WatchUpdate(&client).await {
                Err(e) => {
                    error!("informer WatchUpdate fail with error {:?}", e);
                }
                Ok(()) => break,
            }

            client = match self.GetClient().await {
                None => return Ok(()),
                Some(c) => c,
            };
        }

        return Ok(());
    }

    pub async fn Distribute(&self, event: &DeltaEvent) {
        let handlers = self.handlers.read().unwrap();
        for h in handlers.values().into_iter() {
            h.handle(&self.store, event)
        }
    }
}

#[derive(Debug)]
pub struct InformerInner {
    pub objType: String,
    pub tenant: String,
    pub namespace: String,
    pub opts: ListOption,

    pub revision: AtomicI64,

    pub serverAddresses: Vec<String>,

    pub lastEventHandlerId: AtomicU64,
    pub store: ThreadSafeStore,
    pub handlers: RwLock<BTreeMap<u64, Arc<dyn EventHandler>>>,

    pub closeNotify: Arc<Notify>,
    pub closed: AtomicBool,
    pub listDone: AtomicBool,
}
