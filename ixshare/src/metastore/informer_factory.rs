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

use std::sync::atomic::AtomicBool;
use std::sync::RwLock;
use std::{collections::BTreeMap, ops::Deref, sync::Arc};

use futures::future::join_all;
use tokio::sync::Notify;

use super::informer::{EventHandler, Informer};
use super::selection_predicate::ListOption;
use crate::common::*;

#[derive(Debug)]
pub struct InformerFactoryInner {
    pub addresses: Vec<String>,
    pub tenant: String,
    pub namespace: String,
    pub informers: BTreeMap<String, Informer>,
    pub closed: AtomicBool,
}

#[derive(Debug, Clone)]
pub struct InformerFactory(Arc<RwLock<InformerFactoryInner>>);

impl Deref for InformerFactory {
    type Target = Arc<RwLock<InformerFactoryInner>>;

    fn deref(&self) -> &Arc<RwLock<InformerFactoryInner>> {
        &self.0
    }
}

impl InformerFactory {
    pub async fn New(addresses: Vec<String>, tenant: &str, namespace: &str) -> Result<Self> {
        let inner = InformerFactoryInner {
            addresses: addresses,
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            informers: BTreeMap::new(),
            closed: AtomicBool::new(false),
        };

        return Ok(Self(Arc::new(RwLock::new(inner))));
    }

    pub fn AddInformer(&self, objType: &str, opts: &ListOption) -> Result<()> {
        let mut inner = self.write().unwrap();
        let addresses = inner.addresses.to_vec();
        let informer = Informer::New(addresses, objType, &inner.tenant, &inner.namespace, opts)?;
        inner.informers.insert(objType.to_string(), informer);
        return Ok(());
    }

    pub async fn Process(&self, notify: Arc<Notify>) -> Result<()> {
        let informers: Vec<Informer> = self.read().unwrap().informers.values().cloned().collect();

        let mut futures = Vec::new();

        for i in informers.iter() {
            futures.push(i.Process(notify.clone()));
        }

        join_all(futures).await;
        return Ok(());
    }

    pub fn RemoveInformer(&self, objType: &str) -> Result<()> {
        let mut inner = self.write().unwrap();
        match inner.informers.remove(objType) {
            None => {
                return Err(Error::NotExist(format!(
                    "RemoveInformer doesn't exist {objType}"
                )))
            }
            Some(_) => return Ok(()),
        }
    }

    pub fn GetInformer(&self, objType: &str) -> Result<Informer> {
        let inner = self.read().unwrap();
        match inner.informers.get(objType) {
            None => {
                return Err(Error::NotExist(format!(
                    "GetInformer doesn't exist {objType}"
                )))
            }
            Some(i) => return Ok(i.clone()),
        }
    }

    pub fn Closed(&self) -> bool {
        return self
            .read()
            .unwrap()
            .closed
            .load(std::sync::atomic::Ordering::SeqCst);
    }

    pub async fn AddEventHandler(&self, h: Arc<dyn EventHandler>) -> Result<()> {
        let inner = self.read().unwrap();
        for (_, i) in &inner.informers {
            i.AddEventHandler(h.clone())?;
        }

        return Ok(());
    }

    pub fn Close(&self) -> Result<()> {
        let inner = self.read().unwrap();
        for (_, informer) in &inner.informers {
            informer.Close()?;
        }

        //inner.informers.clear();
        inner
            .closed
            .store(true, std::sync::atomic::Ordering::SeqCst);

        return Ok(());
    }
}
