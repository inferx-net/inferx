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
use serde_json::Value;
use spin::RwLock;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use inferxlib::data_obj::*;
use crate::common::*;

// Store is a generic object storage and processing interface.  A
// Store holds a map from string keys to accumulators, and has
// operations to add, update, and delete a given object to/from the
// accumulator currently associated with a given key.  A Store also
// knows how to extract the key from a given object, so many operations
// are given only the object.
//
// In the simplest Store implementations each accumulator is simply
// the last given object, or empty after Delete, and thus the Store's
// behavior is simple storage.
//
// Reflector knows how to watch a server and update a Store.  This
// package provides a variety of implementations of Store.
pub trait Store {
    // Add adds the given object to the accumulator associated with the given object's key
    fn Add(&self, _obj: &DataObject<Value>) -> Result<()> {
        return Ok(());
    }

    // Update updates the given object in the accumulator associated with the given object's key
    fn Update(&self, _obj: &DataObject<Value>) -> Result<()> {
        todo!();
    }

    // Delete deletes the given object from the accumulator associated with the given object's key
    fn Delete(&self, _obj: &DataObject<Value>) -> Result<()> {
        todo!();
    }

    // List returns a list of all the currently non-empty accumulators
    fn List(&self) -> Vec<DataObject<Value>> {
        todo!();
    }

    // ListKeys returns a list of all the keys currently associated with non-empty accumulators
    fn ListKeys(&self) -> Vec<String> {
        todo!();
    }

    // Get returns the accumulator associated with the given object's key
    fn Get(&self, _key: &str) -> Option<DataObject<Value>> {
        todo!();
    }

    // Replace will delete the contents of the store, using instead the
    // given list. Store takes ownership of the list, you should not reference
    // it after calling this function.
    fn Replace(&self, _objs: &[DataObject<Value>]) -> Result<()> {
        todo!();
    }

    // Resync is meaningless in the terms appearing here but has
    // meaning in some implementations that have non-trivial
    // additional behavior (e.g., DeltaFIFO).
    fn Resync(&self) -> Result<()> {
        todo!();
    }
}

pub static STORE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct ThreadSafeStoreInner {
    pub id: u64,
    pub map: BTreeMap<String, DataObject<Value>>,
}

impl Default for ThreadSafeStoreInner {
    fn default() -> Self {
        let ret = Self {
            id: STORE_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            map: BTreeMap::new(),
        };

        return ret;
    }
}

#[derive(Debug, Default, Clone)]
pub struct ThreadSafeStore(Arc<RwLock<ThreadSafeStoreInner>>);

impl Deref for ThreadSafeStore {
    type Target = Arc<RwLock<ThreadSafeStoreInner>>;

    fn deref(&self) -> &Arc<RwLock<ThreadSafeStoreInner>> {
        &self.0
    }
}

impl ThreadSafeStore {
    pub fn Add(&self, obj: &DataObject<Value>) -> Result<()> {
        let key = obj.Key();
        match self.write().map.insert(key.clone(), obj.clone()) {
            None => return Ok(()),
            Some(_o) => {
                return Err(Error::CommonError(format!(
                    "ThreadSafeStore Add get not none prev obj with obj {:#?}",
                    key
                )));
            }
        }
    }

    pub fn Update(&self, obj: &DataObject<Value>) -> Result<DataObject<Value>> {
        let key = obj.Key();
        match self.write().map.insert(key.clone(), obj.clone()) {
            None => {
                return Err(Error::CommonError(format!(
                    "ThreadSafeStore Update get none prev obj with key {}",
                    key
                )))
            }
            Some(o) => return Ok(o),
        }
    }

    pub fn Delete(&self, obj: &DataObject<Value>) -> Result<DataObject<Value>> {
        let key = obj.Key();
        match self.write().map.remove(&key) {
            None => {
                return Err(Error::CommonError(format!(
                    "ThreadSafeStore Update get none prev obj with key {}",
                    key
                )))
            }
            Some(obj) => return Ok(obj),
        }
    }

    pub fn List(&self) -> Vec<DataObject<Value>> {
        let list: Vec<DataObject<Value>> = self.read().map.values().cloned().collect();
        return list;
    }

    pub fn ListKeys(&self) -> Vec<String> {
        return self.read().map.keys().cloned().collect();
    }

    pub fn Replace(&self, items: &[DataObject<Value>]) -> Result<Vec<DeltaEvent>> {
        let mut inner = self.write();
        let mut events = Vec::new();
        let mut map = BTreeMap::new();
        let initList = inner.map.len() == 0;
        for o in items {
            map.insert(o.Key(), o.clone());
            match inner.map.get(&o.Key()) {
                None => events.push(DeltaEvent {
                    type_: EventType::Added,
                    inInitialList: initList,
                    obj: o.clone(),
                    oldObj: None,
                }),
                Some(obj) => {
                    assert!(o.Revision() >= obj.Revision());
                    if o.Revision() != obj.Revision() {
                        events.push(DeltaEvent {
                            type_: EventType::Modified,
                            inInitialList: false,
                            obj: o.clone(),
                            oldObj: Some(obj.clone()),
                        })
                    }
                }
            }
        }

        for (k, o) in &inner.map {
            match map.get(k) {
                None => events.push(DeltaEvent {
                    type_: EventType::Deleted,
                    inInitialList: false,
                    obj: o.clone(),
                    oldObj: None,
                }),
                Some(_) => (),
            }
        }
        inner.map = map;
        return Ok(events);
    }

    pub fn Get(&self, key: &str) -> Option<DataObject<Value>> {
        return match self.read().map.get(key) {
            None => None,
            Some(v) => Some(v.clone()),
        };
    }
}
