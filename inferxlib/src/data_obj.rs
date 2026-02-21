// Copyright (c) 2025 InferX Authors / 2014 The Kubernetes Authors
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

use serde::Serialize;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::ops::Bound::*;
use std::ops::Deref;
use std::result::Result as SResult;
use std::sync::Arc;
use std::sync::Mutex;

use crate::common::*;

use super::selector::Labels;

pub trait DeepCopy {
    fn DeepCopy(&self) -> Self;
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum EventType {
    None,
    Added,
    Modified,
    Deleted,
    InitDone,
    Error(String),
}

impl EventType {
    pub fn DeepCopy(&self) -> Self {
        match self {
            Self::None => return Self::None,
            Self::Added => return Self::Added,
            Self::Modified => return Self::Modified,
            Self::Deleted => return Self::Deleted,
            Self::InitDone => return Self::InitDone,
            Self::Error(str) => return Self::Error(str.to_string()),
        }
    }
}

impl Default for EventType {
    fn default() -> Self {
        return Self::None;
    }
}

#[derive(Debug, Clone)]
pub struct DeltaEvent {
    pub type_: EventType,
    pub inInitialList: bool,

    pub obj: DataObject<Value>,
    pub oldObj: Option<DataObject<Value>>,
}

#[derive(Debug)]
pub struct WatchEvent {
    pub type_: EventType,

    pub obj: DataObject<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EdgeListGraph<T: Serialize> {
    nodes: T,
}

pub fn validate_name<'de, D>(deserializer: D) -> SResult<String, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;

    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        Ok(s)
    } else {
        Err(serde::de::Error::custom(
            "invalid characters: only 0-9, a-z, A-Z, '-', '_' and '.' allowed",
        ))
    }
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DataObject<SpecType: Serialize + Clone + core::fmt::Debug + Default> {
    #[serde(alias = "type")]
    pub objType: String,
    #[serde(deserialize_with = "validate_name")]
    pub tenant: String,
    #[serde(deserialize_with = "validate_name")]
    pub namespace: String,
    #[serde(deserialize_with = "validate_name")]
    pub name: String,
    #[serde(default)]
    pub labels: Labels,
    #[serde(default)]
    pub annotations: Labels,

    // Revision of the Channel
    #[serde(skip_serializing, default)]
    pub channelRev: i64,

    #[serde(skip_serializing, default)]
    pub srcEpoch: i64,

    // revision number set by creator of object such as etcd
    #[serde(skip_serializing, default)]
    pub revision: i64,

    pub object: SpecType,
}

impl DataObject<Value> {
    pub fn To<T: Serialize + for<'a> Deserialize<'a> + Clone + core::fmt::Debug + Default>(
        &self,
    ) -> Result<DataObject<T>> {
        let o = DataObject {
            objType: self.objType.clone(),
            tenant: self.tenant.clone(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            labels: self.labels.clone(),
            annotations: self.annotations.clone(),
            channelRev: self.channelRev,
            revision: self.revision,
            srcEpoch: self.srcEpoch,
            object: serde_json::from_value::<T>(self.object.clone())?,
        };

        return Ok(o);
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EpochRevision {
    pub srcEpoch: i64,
    pub revision: i64,
}

impl<SpecType: Serialize + for<'a> Deserialize<'a> + Clone + core::fmt::Debug + Default>
    DataObject<SpecType>
{
    pub fn ToJson(&self) -> String {
        return serde_json::to_string(self).unwrap();
    }

    pub fn FromDataObject(obj: DataObject<Value>) -> Result<Self> {
        return obj.To::<SpecType>();
    }

    pub fn DataObject(&self) -> DataObject<Value> {
        let o = DataObject {
            objType: self.objType.clone(),
            tenant: self.tenant.clone(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            labels: self.labels.clone(),
            annotations: self.annotations.clone(),
            channelRev: self.channelRev,
            revision: self.revision,
            srcEpoch: self.srcEpoch,
            object: serde_json::to_value(self.object.clone()).unwrap(),
        };

        return o;
    }

    pub fn CopyWithRev(&self, channelRev: i64, revision: i64) -> Self {
        return Self {
            objType: self.objType.clone(),
            tenant: self.tenant.clone(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            labels: self.labels.Copy(),
            annotations: self.annotations.Copy(),
            channelRev: channelRev,
            revision: revision,
            srcEpoch: self.srcEpoch,
            object: self.object.clone(),
        };
    }

    pub fn NewFromString(s: &str) -> Result<DataObject<SpecType>> {
        let inner: DataObject<SpecType> = serde_json::from_str::<DataObject<SpecType>>(s)?;
        return Ok(inner);
    }
    pub fn Tenant(&self) -> String {
        return self.tenant.clone();
    }

    pub fn Namespace(&self) -> String {
        return self.namespace.clone();
    }

    pub fn Name(&self) -> String {
        return self.name.clone();
    }

    pub fn Key(&self) -> String {
        return format!("{}/{}/{}", &self.tenant, &self.namespace, &self.name);
    }

    pub fn Objectkey(&self) -> String {
        return format!(
            "{}/{}/{}/{}",
            &self.tenant, &self.namespace, &self.name, &self.revision
        );
    }

    pub fn StoreKey(&self) -> String {
        return format!(
            "{}/{}/{}/{}",
            &self.objType, &self.tenant, &self.namespace, &self.name
        );
    }

    pub fn EpochRevision(&self) -> EpochRevision {
        return EpochRevision {
            srcEpoch: self.srcEpoch,
            revision: self.revision,
        };
    }

    pub fn Revision(&self) -> i64 {
        return self.revision;
    }

    pub fn Labels(&self) -> Labels {
        let lables = self.labels.clone();
        return lables;
    }
}

impl DeepCopy for DataObject<Value> {
    fn DeepCopy(&self) -> Self {
        return Self {
            objType: self.objType.clone(),
            tenant: self.tenant.clone(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            labels: self.labels.Copy(),
            annotations: self.annotations.Copy(),
            channelRev: self.channelRev,
            revision: self.revision,
            srcEpoch: self.srcEpoch,
            object: self.object.clone(),
        };
    }
}

#[derive(Debug, Default)]
pub struct DataObjectMgrInner<SpecType: Serialize + Clone + core::fmt::Debug + Default> {
    pub objs: BTreeMap<String, DataObject<SpecType>>,
}

#[derive(Debug, Default, Clone)]
pub struct DataObjectMgr<SpecType: Serialize + Clone + core::fmt::Debug + Default>(
    Arc<Mutex<DataObjectMgrInner<SpecType>>>,
);

impl<SpecType: Serialize + Clone + core::fmt::Debug + Default> Deref for DataObjectMgr<SpecType> {
    type Target = Arc<Mutex<DataObjectMgrInner<SpecType>>>;

    fn deref(&self) -> &Arc<Mutex<DataObjectMgrInner<SpecType>>> {
        &self.0
    }
}

impl<SpecType: Serialize + for<'a> Deserialize<'a> + Clone + core::fmt::Debug + Default>
    DataObjectMgr<SpecType>
{
    pub fn Contains(&self, tenant: &str, namespace: &str, name: &str) -> bool {
        let key = format!("{}/{}/{}", tenant, namespace, name);
        return self.lock().unwrap().objs.contains_key(&key);
    }

    pub fn IsEmpty(&self, tenant: &str, namespace: &str) -> bool {
        let start = if tenant.len() > 0 {
            if namespace.len() > 0 {
                format!("{}/{}/", tenant, namespace)
            } else {
                format!("{}/", tenant)
            }
        } else {
            "".to_owned()
        };
        for (key, _) in self
            .lock()
            .unwrap()
            .objs
            .range::<String, _>((Included(start.clone()), Unbounded))
        {
            if key.starts_with(&start) {
                return false;
            } else {
                break;
            }
        }

        return true;
    }

    pub fn GetObjectKeys(&self, tenant: &str, namespace: &str) -> Result<Vec<String>> {
        let start = if tenant.len() > 0 {
            if namespace.len() > 0 {
                format!("{}/{}/", tenant, namespace)
            } else {
                format!("{}/", tenant)
            }
        } else {
            "".to_owned()
        };
        let mut vec = Vec::new();
        for (key, _) in self
            .lock()
            .unwrap()
            .objs
            .range::<String, _>((Included(start.clone()), Unbounded))
        {
            if key.starts_with(&start) {
                vec.push(key.clone());
            } else {
                break;
            }
        }

        return Ok(vec);
    }

    pub fn GetObjectsByPrefix(
        &self,
        tenant: &str,
        namespace: &str,
        nameprefix: &str,
    ) -> Result<Vec<DataObject<SpecType>>> {
        let start = if tenant.len() == 0 {
            "".to_owned()
        } else if namespace.len() == 0 {
            format!("{}/", tenant)
        } else {
            format!("{}/{}/{}", tenant, namespace, nameprefix)
        };

        let mut vec = Vec::new();
        for (key, o) in self
            .lock()
            .unwrap()
            .objs
            .range::<String, _>((Included(start.clone()), Unbounded))
        {
            if key.starts_with(&start) {
                vec.push(o.clone());
            } else {
                break;
            }
        }

        return Ok(vec);
    }

    pub fn GetObjects(&self, tenant: &str, namespace: &str) -> Result<Vec<DataObject<SpecType>>> {
        let start = if tenant.len() > 0 {
            if namespace.len() > 0 {
                format!("{}/{}/", tenant, namespace)
            } else {
                format!("{}/", tenant)
            }
        } else {
            "".to_owned()
        };
        let mut vec = Vec::new();
        for (key, o) in self
            .lock()
            .unwrap()
            .objs
            .range::<String, _>((Included(start.clone()), Unbounded))
        {
            if key.starts_with(&start) {
                vec.push(o.clone());
            } else {
                break;
            }
        }

        return Ok(vec);
    }

    pub fn GetByObjectkey(&self, key: &str) -> Result<DataObject<SpecType>> {
        match self.lock().unwrap().objs.get(key) {
            None => {
                return Err(Error::NotExist(format!("GetByObjectkey::get {}", key)));
            }
            Some(o) => return Ok(o.clone()),
        }
    }

    pub fn Get(&self, tenant: &str, namespace: &str, name: &str) -> Result<DataObject<SpecType>> {
        let key = format!("{}/{}/{}", tenant, namespace, name);
        let inner = self.lock().unwrap();

        match inner.objs.get(&key) {
            None => {
                return Err(Error::NotExist(format!(
                    "DataObjectMgr::get {} keyes {:#?}",
                    key,
                    inner.objs.keys()
                )));
            }
            Some(o) => return Ok(o.clone()),
        }
    }

    pub fn Add(&self, obj: DataObject<SpecType>) -> Result<()> {
        let mut inner = self.lock().unwrap();

        let key = obj.Key();

        if inner.objs.contains_key(&key) {
            return Err(Error::Exist(format!("DataObjectMgr::Add {}", &key)));
        };

        inner.objs.insert(key, obj);

        return Ok(());
    }

    pub fn Update(&self, obj: DataObject<SpecType>) -> Result<()> {
        let mut inner = self.lock().unwrap();

        let key = obj.Key();

        if !inner.objs.contains_key(&key) {
            return Err(Error::NotExist(format!("DataObjectMgr::Update {}", &key)));
        };

        inner.objs.insert(key, obj);

        return Ok(());
    }

    pub fn Remove(&self, obj: DataObject<SpecType>) -> Result<()> {
        let key = obj.Key();
        let mut inner = self.lock().unwrap();
        if !inner.objs.contains_key(&key) {
            return Err(Error::NotExist(format!(
                "DataObjectMgr::Remove {}/{:?}",
                key,
                inner.objs.keys()
            )));
        }

        inner.objs.remove(&key);

        return Ok(());
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ObjLink {
    pub objType: String,
    pub namespace: String,
    pub name: String,
}

impl ObjLink {
    pub fn Key(&self, tenant: &str) -> String {
        return format!("{}/{}/{}", tenant, &self.namespace, &self.name);
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ObjRef<SpecType: Serialize + Clone + core::fmt::Debug + Default> {
    Obj(SpecType),
    Link(ObjLink),
}

impl<SpecType> Default for ObjRef<SpecType>
where
    SpecType: Serialize + Clone + core::fmt::Debug + Default,
{
    fn default() -> Self {
        Self::Obj(SpecType::default())
    }
}
