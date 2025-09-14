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

use std::collections::BTreeMap;
use std::ops::Deref;
use std::result::Result as SResult;
use std::sync::Arc;
use std::sync::RwLock;

use inferxlib::selector::Selector;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use crate::metastore::cache_store::CacheStore;
use crate::metastore::cache_store::ChannelRev;
use crate::metastore::obj::ToObject;
use crate::metastore::selection_predicate::*;
use crate::ixmeta;
use inferxlib::data_obj::*;

#[derive(Debug, Default, Clone)]
pub struct SvcDir(Arc<RwLock<SvcDirInner>>);

impl Deref for SvcDir {
    type Target = Arc<RwLock<SvcDirInner>>;

    fn deref(&self) -> &Arc<RwLock<SvcDirInner>> {
        &self.0
    }
}

impl SvcDir {
    pub fn GetCacher(&self, objType: &str) -> Option<CacheStore> {
        return match self.read().unwrap().map.get(objType) {
            None => None,
            Some(c) => Some(c.clone()),
        };
    }

    pub fn AddCacher(&self, cacher: CacheStore) {
        self.write().unwrap().map.insert(cacher.ObjType(), cacher);
    }

    pub fn ChannelRev(&self) -> ChannelRev {
        return self.read().unwrap().channelRev.clone();
    }

    pub fn ObjectTypes(&self) -> Vec<String> {
        return self.read().unwrap().map.keys().cloned().collect();
    }
}

#[derive(Debug, Default)]
pub struct SvcDirInner {
    pub map: BTreeMap<String, CacheStore>,
    pub channelRev: ChannelRev,
    pub version: String,
}

#[tonic::async_trait]
impl ixmeta::ix_meta_service_server::IxMetaService for SvcDir {
    async fn version(
        &self,
        request: Request<ixmeta::VersionRequestMessage>,
    ) -> SResult<Response<ixmeta::VersionResponseMessage>, Status> {
        error!("Request from {:?}", request.remote_addr());

        let response = ixmeta::VersionResponseMessage {
            version: self.read().unwrap().version.clone(),
        };
        Ok(Response::new(response))
    }

    async fn create(
        &self,
        _request: Request<ixmeta::CreateRequestMessage>,
    ) -> SResult<Response<ixmeta::CreateResponseMessage>, Status> {
        unimplemented!()
    }

    async fn update(
        &self,
        _request: Request<ixmeta::UpdateRequestMessage>,
    ) -> SResult<Response<ixmeta::UpdateResponseMessage>, Status> {
        unimplemented!()
    }

    async fn uid(
        &self,
        _request: Request<ixmeta::UidRequestMessage>,
    ) -> SResult<Response<ixmeta::UidReponseMessage>, Status> {
        unimplemented!()
    }

    async fn delete(
        &self,
        _request: Request<ixmeta::DeleteRequestMessage>,
    ) -> SResult<Response<ixmeta::DeleteResponseMessage>, Status> {
        unimplemented!()
    }

    async fn get(
        &self,
        request: Request<ixmeta::GetRequestMessage>,
    ) -> SResult<Response<ixmeta::GetResponseMessage>, Status> {
        let req = request.get_ref();
        let cacher = match self.GetCacher(&req.obj_type) {
            None => {
                return Ok(Response::new(ixmeta::GetResponseMessage {
                    error: format!("svcdir get doesn't support obj type {}", &req.obj_type),
                    obj: None,
                }))
            }
            Some(c) => c,
        };

        match cacher
            .Get(&req.tenant, &req.namespace, &req.name, req.revision)
            .await
        {
            Err(e) => {
                return Ok(Response::new(ixmeta::GetResponseMessage {
                    error: format!("Fail: {:?}", e),
                    obj: None,
                }))
            }
            Ok(o) => {
                return Ok(Response::new(ixmeta::GetResponseMessage {
                    error: "".into(),
                    obj: match o {
                        None => None,
                        Some(o) => Some(o.Obj()),
                    },
                }))
            }
        }
    }

    async fn list(
        &self,
        request: Request<ixmeta::ListRequestMessage>,
    ) -> SResult<Response<ixmeta::ListResponseMessage>, Status> {
        let req = request.get_ref();
        let cacher = match self.GetCacher(&req.obj_type) {
            None => {
                return Ok(Response::new(ixmeta::ListResponseMessage {
                    error: format!("svcdir list doesn't support obj type {}", &req.obj_type),
                    revision: 0,
                    objs: Vec::new(),
                }))
            }
            Some(c) => c,
        };

        let labelSelector = match Selector::Parse(&req.label_selector) {
            Err(e) => {
                return Ok(Response::new(ixmeta::ListResponseMessage {
                    error: format!("Fail: {:?}", e),
                    ..Default::default()
                }))
            }
            Ok(s) => s,
        };
        let fieldSelector = match Selector::Parse(&req.field_selector) {
            Err(e) => {
                return Ok(Response::new(ixmeta::ListResponseMessage {
                    error: format!("Fail: {:?}", e),
                    ..Default::default()
                }))
            }
            Ok(s) => s,
        };

        let opts = ListOption {
            revision: req.revision,
            revisionMatch: RevisionMatch::Exact,
            predicate: SelectionPredicate {
                label: labelSelector,
                field: fieldSelector,
                limit: 00,
                continue_: None,
            },
        };

        match cacher.List(&req.tenant, &req.namespace, &opts).await {
            Err(e) => {
                return Ok(Response::new(ixmeta::ListResponseMessage {
                    error: format!("Fail: {:?}", e),
                    ..Default::default()
                }))
            }
            Ok(resp) => {
                let mut objs = Vec::new();
                for o in resp.objs {
                    objs.push(o.Obj());
                }
                return Ok(Response::new(ixmeta::ListResponseMessage {
                    error: "".into(),
                    revision: resp.revision,
                    objs: objs,
                }));
            }
        }
    }

    type WatchStream =
        std::pin::Pin<Box<dyn futures::Stream<Item = SResult<ixmeta::WEvent, Status>> + Send>>;

    async fn watch(
        &self,
        request: Request<ixmeta::WatchRequestMessage>,
    ) -> SResult<Response<Self::WatchStream>, Status> {
        let (tx, rx) = mpsc::channel(200);
        let stream = ReceiverStream::new(rx);

        let svcDir = self.clone();
        tokio::spawn(async move {
            let req = request.get_ref();
            let cacher = match svcDir.GetCacher(&req.obj_type) {
                None => {
                    tx.send(Err(Status::invalid_argument(&format!(
                        "doesn't support obj type {}",
                        &req.obj_type
                    ))))
                    .await
                    .ok();
                    return;
                }
                Some(c) => c,
            };

            let labelSelector = match Selector::Parse(&req.label_selector) {
                Err(e) => {
                    tx.send(Err(Status::invalid_argument(&format!("Fail: {:?}", e))))
                        .await
                        .ok();

                    return;
                }
                Ok(s) => s,
            };
            let fieldSelector = match Selector::Parse(&req.field_selector) {
                Err(e) => {
                    tx.send(Err(Status::invalid_argument(&format!("Fail: {:?}", e))))
                        .await
                        .ok();
                    return;
                }
                Ok(s) => s,
            };

            let predicate = SelectionPredicate {
                label: labelSelector,
                field: fieldSelector,
                limit: 00,
                continue_: None,
            };

            match cacher.Watch(&req.namespace, req.revision, predicate) {
                Err(e) => {
                    tx.send(Err(Status::invalid_argument(&format!("Fail: {:?}", e))))
                        .await
                        .ok();
                    return;
                }
                Ok(mut w) => loop {
                    let event = w.stream.recv().await;
                    match event {
                        None => return,
                        Some(event) => {
                            let eventType = match event.type_ {
                                EventType::None => 0,
                                EventType::Added => 1,
                                EventType::Modified => 2,
                                EventType::Deleted => 3,
                                EventType::InitDone => 4,
                                EventType::Error(s) => {
                                    tx.send(Err(Status::invalid_argument(&format!(
                                        "Fail: {:?}",
                                        s
                                    ))))
                                    .await
                                    .ok();
                                    return;
                                }
                            };

                            let we = ixmeta::WEvent {
                                event_type: eventType,
                                obj: Some(event.obj.Obj()),
                            };
                            match tx.send(Ok(we)).await {
                                Ok(()) => (),
                                Err(e) => {
                                    tx.send(Err(Status::invalid_argument(&format!(
                                        "Fail: {:?}",
                                        e
                                    ))))
                                    .await
                                    .ok();
                                    return;
                                }
                            }
                        }
                    }
                },
            }
        });

        return Ok(Response::new(Box::pin(stream) as Self::WatchStream));
    }
}
