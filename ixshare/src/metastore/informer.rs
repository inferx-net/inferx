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
use std::time::Duration;
use std::{collections::BTreeMap, fmt::Debug, sync::Arc};
use tokio::sync::RwLock;

use async_trait::async_trait;

use tokio::sync::Notify;

use rand::Rng;

use super::cacher_client::*;
use super::selection_predicate::*;
use super::store::ThreadSafeStore;
use crate::common::*;
use inferxlib::data_obj::*;

#[async_trait]
pub trait EventHandler: Debug + Send + Sync {
    async fn handle(&self, store: &ThreadSafeStore, event: &DeltaEvent);
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

    pub async fn AddEventHandler(&self, h: Arc<dyn EventHandler>) -> Result<u64> {
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

            h.handle(&self.store, &event).await;
        }

        self.handlers.write().await.insert(id, h.clone());

        return Ok(id);
    }

    pub async fn RemoveEventHandler(&mut self, id: u64) -> Option<Arc<dyn EventHandler>> {
        return self.handlers.write().await.remove(&id);
    }

    pub async fn Connect(&self, addr: &str) -> Result<CacherClient> {
        let client = CacherClient::New(addr.to_owned()).await?;
        return Ok(client);
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

            error!("informer::GetClient fail, waiting 1000 ms to retry");
            // retry after one second
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(1000)) => {}
                _ = self.closeNotify.notified() => {
                    self.closed.store(true, Ordering::SeqCst);
                    return None
                }
            }
            error!("informer::GetClient fail, retrying");
        }
    }

    async fn Merge(&self, first: bool, newstore: &ThreadSafeStore) -> Result<Vec<DeltaEvent>> {
        let mut map = BTreeMap::new();
        let mut l = self.store.write();
        let newl = newstore.write();
        for (k, v) in &newl.map {
            match l.map.get(k) {
                None => {
                    map.insert(
                        v.EpochRevision(),
                        DeltaEvent {
                            type_: EventType::Added,
                            inInitialList: first,
                            obj: v.clone(),
                            oldObj: None,
                        },
                    );
                }
                Some(old) => {
                    if old.EpochRevision() < v.EpochRevision() {
                        // the new obj is newer than the saved
                        map.insert(
                            v.EpochRevision(),
                            DeltaEvent {
                                type_: EventType::Modified,
                                inInitialList: first,
                                obj: v.clone(),
                                oldObj: Some(old.clone()),
                            },
                        );
                    }
                }
            }
        }

        for (k, v) in &l.map {
            match newl.map.get(k) {
                None => {
                    map.insert(
                        v.EpochRevision(),
                        DeltaEvent {
                            type_: EventType::Deleted,
                            inInitialList: false,
                            obj: v.clone(),
                            oldObj: None,
                        },
                    );
                }
                Some(_) => (),
            }
        }

        l.map.clear();
        for (k, v) in &newl.map {
            l.map.insert(k.clone(), v.clone());
        }

        l.id = newl.id;
        return Ok(map.values().cloned().collect());
    }

    async fn InitList(&self, client: &CacherClient, first: bool) -> Result<()> {
        // let store = self.store.clone();

        let objType = self.objType.clone();
        let tenant = self.tenant.clone();
        let namespace = self.namespace.clone();
        let opts = self.opts.DeepCopy();

        let objs = client.List(&objType, &tenant, &namespace, &opts).await?;
        self.revision.store(objs.revision, Ordering::SeqCst);

        let store = ThreadSafeStore::default();
        for o in objs.objs {
            store.Add(&o)?;
        }

        let events = self.Merge(first, &store).await?;

        info!(
            "InitList first {} {} event {:#?}",
            &objType,
            first,
            events.len()
        );

        for e in &events {
            self.Distribute(e).await;
        }

        if first {
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
        }

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
                        let current_revision = self.revision.load(Ordering::Acquire);
                        let err_str = format!("{:?}", e);

                        error!(
                            "WatchUpdate type is {}/{} watch get error {:?}",
                            self.objType,
                            current_revision,
                            e
                        );

                        // Log additional details if this is a "too old resource version" error
                        if err_str.contains("too old resource version") {
                            error!(
                                "DIAGNOSE: Too old resource version detected - objType={}, requested_revision={}, error_details={}",
                                self.objType,
                                current_revision,
                                err_str
                            );
                        }

                        break;
                    }
                    Ok(e) => match e {
                        None => break,
                        Some(e) => {
                            opts.revision = e.obj.channelRev;
                            self.revision.store(opts.revision, Ordering::SeqCst);
                            let oldobj = store.Get(&e.obj.Key());
                            if let Some(old) = oldobj {
                                if old.revision >= e.obj.Revision() {
                                    continue;
                                }
                            }

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
        }
    }

    pub async fn Load(&self, addr: &str, first: bool) -> Result<()> {
        let client = self.Connect(addr).await?;

        match self.InitList(&client, first).await {
            Err(e) => {
                error!("informer initlist fail with error {:?}", e);
                return Err(e);
            }
            Ok(()) => (),
        }

        if first {
            self.listDone.store(true, Ordering::SeqCst);
        }

        return Ok(());
    }

    pub async fn UpdateProcess(&self, addr: &str) -> Result<()> {
        let client = self.Connect(addr).await?;
        loop {
            match self.WatchUpdate(&client).await {
                Err(e) => {
                    error!("informer UpdateProcess fail with error {:?}", e);
                    break;
                }
                Ok(()) => continue,
            }
        }

        return Ok(());
    }

    pub async fn Process(&self, notify: Arc<Notify>) -> Result<()> {
        let mut client = match self.GetClient().await {
            None => return Ok(()),
            Some(c) => c,
        };

        loop {
            match self.InitList(&client, true).await {
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

        let watch_start_rev = self.revision.load(Ordering::Acquire);
        if self.objType == "node" {
            error!("Informer::Process [{}] InitList done, starting WatchUpdate from revision {}",
                self.objType, watch_start_rev);
        }

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
        let handlers = self.handlers.read().await;
        for h in handlers.values().into_iter() {
            h.handle(&self.store, event).await
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
