use std::collections::BTreeMap;

use crate::common::*;
use crate::ixmeta::*;
use inferxlib::selector::Labels;
use prost::Message;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use inferxlib::data_obj::DataObject;

pub trait ToKvVec {
    fn ToVec(&self) -> Vec<Kv>;
}

impl ToKvVec for Labels {
    fn ToVec(&self) -> Vec<Kv> {
        let mut ret = Vec::with_capacity(self.0.len());
        for (k, v) in &self.0 {
            let kv = Kv {
                key: k.clone(),
                val: v.clone(),
            };
            ret.push(kv);
        }

        return ret;
    }
}

pub trait ToObject {
    fn Object(&self) -> Object;
    fn Obj(&self) -> Obj;
    fn Encode(&self) -> Result<Vec<u8>>;

    fn NewFromObj<
        SpecType: Serialize + for<'a> Deserialize<'a> + Clone + core::fmt::Debug + Default,
    >(
        item: &Obj,
    ) -> DataObject<SpecType>;

    fn NewFromObject<
        SpecType: Serialize + for<'a> Deserialize<'a> + Clone + core::fmt::Debug + Default,
    >(
        item: &Object,
        channelRev: i64,
        revision: i64,
    ) -> DataObject<SpecType>;
}

impl<SpecType: Serialize + for<'a> Deserialize<'a> + Clone + core::fmt::Debug + Default> ToObject
    for DataObject<SpecType>
{
    fn Object(&self) -> Object {
        return Object {
            kind: self.objType.clone(),
            tenant: self.tenant.clone(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            labels: self.labels.ToVec(),
            annotations: self.annotations.ToVec(),
            data: serde_json::to_string(&self.object).unwrap(),
        };
    }

    fn Obj(&self) -> Obj {
        return Obj {
            kind: self.objType.clone(),
            tenant: self.tenant.clone(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            channel_rev: self.channelRev,
            revision: self.Revision(),
            labels: self.labels.ToVec(),
            annotations: self.annotations.ToVec(),
            data: serde_json::to_string(&self.object).unwrap(),
        };
    }

    fn Encode(&self) -> Result<Vec<u8>> {
        let mut buf: Vec<u8> = Vec::new();
        let obj = self.Object();
        buf.reserve(obj.encoded_len());
        obj.encode(&mut buf)?;
        return Ok(buf);
    }

    fn NewFromObj<
        SpecType1: Serialize + for<'a> Deserialize<'a> + Clone + core::fmt::Debug + Default,
    >(
        item: &Obj,
    ) -> DataObject<SpecType1> {
        let mut lables = BTreeMap::new();
        for l in &item.labels {
            lables.insert(l.key.clone(), l.val.clone());
        }

        let mut annotations = BTreeMap::new();
        for l in &item.annotations {
            annotations.insert(l.key.clone(), l.val.clone());
        }

        let inner = DataObject {
            objType: item.kind.clone(),
            tenant: item.tenant.clone(),
            namespace: item.namespace.clone(),
            name: item.name.clone(),
            labels: lables.into(),
            annotations: annotations.into(),
            channelRev: item.channel_rev,
            revision: item.revision,
            object: serde_json::from_str::<SpecType1>(&item.data).unwrap(),
        };

        return inner;
    }

    fn NewFromObject<
        SpecType1: Serialize + for<'a> Deserialize<'a> + Clone + core::fmt::Debug + Default,
    >(
        item: &Object,
        channelRev: i64,
        revision: i64,
    ) -> DataObject<SpecType1> {
        let mut lables = BTreeMap::new();
        for l in &item.labels {
            lables.insert(l.key.clone(), l.val.clone());
        }

        let mut annotations = BTreeMap::new();
        for l in &item.annotations {
            annotations.insert(l.key.clone(), l.val.clone());
        }

        let inner = DataObject {
            objType: item.kind.clone(),
            tenant: item.tenant.clone(),
            namespace: item.namespace.clone(),
            name: item.name.clone(),
            labels: lables.into(),
            annotations: annotations.into(),
            channelRev: channelRev,
            revision: revision,
            object: serde_json::from_str::<SpecType1>(&item.data).unwrap(),
        };

        return inner;
    }
}

impl From<&Object> for DataObject<Value> {
    fn from(item: &Object) -> Self {
        let mut lables = BTreeMap::new();
        for l in &item.labels {
            lables.insert(l.key.clone(), l.val.clone());
        }

        let mut annotations = BTreeMap::new();
        for l in &item.annotations {
            annotations.insert(l.key.clone(), l.val.clone());
        }

        let inner = DataObject {
            objType: item.kind.clone(),
            tenant: item.tenant.clone(),
            namespace: item.namespace.clone(),
            name: item.name.clone(),
            labels: lables.into(),
            annotations: annotations.into(),
            channelRev: 0,
            revision: 0,
            object: serde_json::from_str(&item.data).unwrap(),
        };

        return inner;
    }
}

impl From<&Obj> for DataObject<Value> {
    fn from(item: &Obj) -> Self {
        let mut lables = BTreeMap::new();
        for l in &item.labels {
            lables.insert(l.key.clone(), l.val.clone());
        }

        let mut annotations = BTreeMap::new();
        for l in &item.annotations {
            annotations.insert(l.key.clone(), l.val.clone());
        }

        let inner = DataObject {
            objType: item.kind.clone(),
            tenant: item.tenant.clone(),
            namespace: item.namespace.clone(),
            name: item.name.clone(),
            labels: lables.into(),
            annotations: annotations.into(),
            channelRev: item.channel_rev,
            revision: item.revision,
            object: serde_json::from_str(&item.data).unwrap(),
        };

        return inner;
    }
}
