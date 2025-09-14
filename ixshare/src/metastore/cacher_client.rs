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
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex as TMutex;
use tonic::transport::Channel;
use tonic::Request;
use tonic::Streaming;

use crate::metastore::obj::ToObject;
// use crate::object_mgr::ObjectMeta;
use super::selection_predicate::ListOption;
use crate::common::*;
use crate::etcd::watch::DataObjList;
use crate::ixmeta::ix_meta_service_client::IxMetaServiceClient;
use crate::ixmeta::*;
use inferxlib::data_obj::*;

#[derive(Debug)]
pub struct ObjectMeta {
    pub name: String,
    pub size: i32,
}

#[derive(Debug, Clone)]
pub struct CacherClient(Arc<TMutex<CacherClientInner>>);

impl Deref for CacherClient {
    type Target = Arc<TMutex<CacherClientInner>>;

    fn deref(&self) -> &Arc<TMutex<CacherClientInner>> {
        &self.0
    }
}

impl CacherClient {
    pub async fn New(ixmetaSvcAddr: String) -> Result<Self> {
        let inner = CacherClientInner::New(ixmetaSvcAddr).await?;
        return Ok(Self(Arc::new(TMutex::new(inner))));
    }

    pub async fn Create(&self, obj: &DataObject<Value>) -> Result<i64> {
        let mut inner = self.lock().await;
        return inner.Create(obj).await;
    }

    pub async fn Update(&self, obj: &DataObject<Value>, expectRev: i64) -> Result<i64> {
        let mut inner = self.lock().await;
        return inner.Update(obj, expectRev).await;
    }

    pub async fn Get(
        &self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        name: &str,
        revision: i64,
    ) -> Result<Option<DataObject<Value>>> {
        let mut inner = self.lock().await;
        return inner.Get(objType, tenant, namespace, name, revision).await;
    }

    pub async fn Delete(
        &self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        name: &str,
        revision: i64,
    ) -> Result<i64> {
        let mut inner = self.lock().await;
        return inner
            .Delete(objType, tenant, namespace, name, revision)
            .await;
    }

    pub async fn List(
        &self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        opts: &ListOption,
    ) -> Result<DataObjList> {
        let mut inner = self.lock().await;
        return inner.List(objType, tenant, namespace, opts).await;
    }

    pub async fn Watch(
        &self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        opts: &ListOption,
    ) -> Result<WatchStream> {
        let mut inner = self.lock().await;
        return inner.Watch(objType, tenant, namespace, opts).await;
    }

    pub async fn Uid(&self) -> Result<i64> {
        let mut inner = self.lock().await;
        return inner.Uid().await;
    }
}

#[derive(Debug)]
pub struct CacherClientInner {
    pub client: IxMetaServiceClient<Channel>,
}

impl CacherClientInner {
    pub async fn New(ixmetaSvcAddr: String) -> Result<Self> {
        let client = IxMetaServiceClient::connect(ixmetaSvcAddr).await?;
        return Ok(Self { client: client });
    }

    pub async fn Create(&mut self, obj: &DataObject<Value>) -> Result<i64> {
        let o = obj.Obj();
        let req = CreateRequestMessage { obj: Some(o) };

        let mut response = self.client.create(Request::new(req)).await?;
        let resp = response.get_mut();
        if resp.error.len() == 0 {
            return Ok(resp.revision);
        }

        return Err(Error::CommonError(resp.error.clone()));
    }

    pub async fn Delete(
        &mut self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        name: &str,
        expectRev: i64,
    ) -> Result<i64> {
        let req = DeleteRequestMessage {
            obj_type: objType.to_owned(),
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            expect_rev: expectRev,
        };

        let mut response = self.client.delete(Request::new(req)).await?;
        let resp = response.get_mut();
        if resp.error.len() == 0 {
            return Ok(resp.revision);
        }

        return Err(Error::CommonError(resp.error.clone()));
    }

    pub async fn Update(&mut self, obj: &DataObject<Value>, expectRev: i64) -> Result<i64> {
        let o = obj.Obj();
        let req = UpdateRequestMessage {
            expect_rev: expectRev,
            obj: Some(o),
        };

        let mut response = self.client.update(Request::new(req)).await?;
        let resp = response.get_mut();
        if resp.error.len() == 0 {
            return Ok(resp.revision);
        }

        return Err(Error::CommonError(resp.error.clone()));
    }

    pub async fn Get(
        &mut self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        name: &str,
        revision: i64,
    ) -> Result<Option<DataObject<Value>>> {
        let req = GetRequestMessage {
            obj_type: objType.to_owned(),
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            revision: revision,
        };

        let mut response = self.client.get(Request::new(req)).await?;
        let resp = response.get_mut();
        if resp.error.len() == 0 {
            match resp.obj.take() {
                None => return Ok(None),
                Some(o) => return Ok(Some(DataObject::<Value>::NewFromObj(&o))),
            }
        }

        return Err(Error::CommonError(resp.error.clone()));
    }

    pub async fn Uid(&mut self) -> Result<i64> {
        let request: UidRequestMessage = UidRequestMessage {};
        let mut response = self.client.uid(request).await?;
        let resp = response.get_mut();
        if resp.error.len() == 0 {
            return Ok(resp.uid);
        }

        return Err(Error::CommonError(resp.error.clone()));
    }

    pub async fn List(
        &mut self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        opts: &ListOption,
    ) -> Result<DataObjList> {
        let req = ListRequestMessage {
            obj_type: objType.to_owned(),
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            revision: opts.revision,
            label_selector: opts.predicate.label.String(),
            field_selector: opts.predicate.field.String(),
        };

        let response = self.client.list(Request::new(req)).await?;
        let resp = response.into_inner();
        if resp.error.len() == 0 {
            let mut objs = Vec::new();
            for dataO in &resp.objs {
                let obj: DataObject<Value> = DataObject::<Value>::NewFromObj(dataO);
                objs.push(obj)
            }

            let dol = DataObjList {
                objs: objs,
                revision: resp.revision,
                ..Default::default()
            };

            return Ok(dol);
        }

        return Err(Error::CommonError(resp.error.clone()));
    }

    pub async fn Watch(
        &mut self,
        objType: &str,
        tenant: &str,
        namespace: &str,
        opts: &ListOption,
    ) -> Result<WatchStream> {
        let req = WatchRequestMessage {
            obj_type: objType.to_owned(),
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            revision: opts.revision,
            label_selector: opts.predicate.label.String(),
            field_selector: opts.predicate.field.String(),
        };

        let response = self.client.watch(Request::new(req)).await?;
        let resp = response.into_inner();
        return Ok(WatchStream { stream: resp });
    }
}

pub struct WatchStream {
    stream: Streaming<WEvent>,
}

impl WatchStream {
    pub async fn Next(&mut self) -> Result<Option<WatchEvent>> {
        let event = self.stream.message().await?;
        match event {
            None => return Ok(None),
            Some(e) => {
                let eventType = match e.event_type {
                    0 => EventType::None,
                    1 => EventType::Added,
                    2 => EventType::Modified,
                    3 => EventType::Deleted,
                    _ => {
                        return Err(Error::CommonError(format!(
                            "invalid watch response type {}",
                            e.event_type
                        )))
                    }
                };

                if e.obj.is_none() {
                    return Err(Error::CommonError(format!(
                        "invalid watch response ob is none"
                    )));
                }

                let watchEvent = WatchEvent {
                    type_: eventType,
                    obj: DataObject::<Value>::NewFromObj(&e.obj.unwrap()),
                };

                return Ok(Some(watchEvent));
            }
        }
    }
}
