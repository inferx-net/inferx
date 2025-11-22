// Copyright (c) 2025 InferX Authors
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
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

//use futures_util::TryStreamExt;
use serde::Deserialize;
use serde::Serialize;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgListener;
use sqlx::postgres::PgPoolOptions;
use sqlx::ConnectOptions;
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::sync::Mutex as TMutext;
use tokio::sync::Notify;

use crate::common::*;

pub type WatcherId = u64;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PgNotification {
    pub process_id: u64,
    pub channel: String,
    pub payload: serde_json::value::Value,
}

pub type Predicate = fn(&serde_json::value::Value) -> bool;

#[derive(Debug)]
pub struct Watcher {
    pub recv: mpsc::Receiver<serde_json::value::Value>,
    pub stream: WatchStream,
    pub listener: Listener,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let listener = self.listener.clone();
        let id = self.stream.id;
        tokio::spawn(async move {
            listener.RemoveWatch(id).await.ok();
        });
    }
}

impl Watcher {
    pub fn New(listener: &Listener, id: u64, predicate: Predicate) -> Result<(Self, WatchStream)> {
        let (sender, recv) = mpsc::channel::<serde_json::value::Value>(1000);
        let stream = WatchStream::New(id, sender, predicate);
        let w = Watcher {
            recv: recv,
            stream: stream.clone(),
            listener: listener.clone(),
        };

        return Ok((w, stream));
    }

    pub async fn Recv(&mut self) -> Result<serde_json::value::Value> {
        if self.stream.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::CommonError(format!("Watcher::closed")));
        }

        let notify = self.stream.closeNotify.clone();
        tokio::select! {
            _ = notify.notified() => {
                return Err(Error::CommonError(format!("Watcher::closed")));
            }
            n = self.recv.recv() => {
                match n {
                    None => {
                        return Err(Error::CommonError(format!("Watcher::closed")));
                    }
                    Some(notification) => {
                        return Ok(notification)
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct WatchStreamInner {
    pub closeNotify: Arc<Notify>,
    pub closed: AtomicBool,
    pub id: u64,
    pub sender: mpsc::Sender<serde_json::value::Value>,
    pub predicate: Predicate,
}

#[derive(Debug, Clone)]
pub struct WatchStream(Arc<WatchStreamInner>);

impl Deref for WatchStream {
    type Target = Arc<WatchStreamInner>;

    fn deref(&self) -> &Arc<WatchStreamInner> {
        &self.0
    }
}

impl WatchStream {
    pub fn New(
        id: u64,
        sender: mpsc::Sender<serde_json::value::Value>,
        predicate: Predicate,
    ) -> Self {
        let inner = WatchStreamInner {
            closeNotify: Arc::new(Notify::new()),
            closed: AtomicBool::new(false),
            id: id,
            sender: sender,
            predicate: predicate,
        };

        return Self(Arc::new(inner));
    }
    pub fn Send(&self, v: &serde_json::value::Value) -> Result<()> {
        if !(self.predicate)(v) {
            return Ok(());
        }
        match self.sender.try_send(v.clone()) {
            Ok(()) => return Ok(()),
            Err(_) => {
                error!("send value to Watcher [{}] fail", self.id);
                self.closeNotify.notify_waiters();
                self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
                return Err(Error::CommonError(format!(
                    "send value to Watcher [{}] fail",
                    self.id
                )));
            }
        }
    }
}

#[derive(Debug)]
pub struct ListenerInner {
    pub closeNotify: Arc<Notify>,
    pub closed: AtomicBool,

    pub pool: PgPool,
    pub channel: String,
    pub nextId: AtomicU64,
    pub watchers: TMutext<HashMap<u64, WatchStream>>,
}

#[derive(Debug, Clone)]
pub struct Listener(Arc<ListenerInner>);

impl Deref for Listener {
    type Target = Arc<ListenerInner>;

    fn deref(&self) -> &Arc<ListenerInner> {
        &self.0
    }
}

impl Listener {
    pub async fn New(sqlSvcAddr: &str, channel: &str) -> Result<Self> {
        let url_parts = url::Url::parse(sqlSvcAddr).expect("Failed to parse URL");
        let username = url_parts.username();
        let password = url_parts.password().unwrap_or("");
        let host = url_parts.host_str().unwrap_or("localhost");
        let port = url_parts.port().unwrap_or(5432);
        let database = url_parts.path().trim_start_matches('/');

        let options = PgConnectOptions::new()
            .host(host)
            .port(port)
            .username(username)
            .password(password)
            .database(database);

        options.clone().disable_statement_logging();

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        return Self::NewWithPool(pool, channel).await;
    }

    pub async fn NewWithPool(pool: PgPool, channel: &str) -> Result<Self> {
        let inner = ListenerInner {
            closeNotify: Arc::new(Notify::new()),
            closed: AtomicBool::new(false),
            pool: pool,
            channel: channel.to_owned(),
            nextId: AtomicU64::new(0),
            watchers: TMutext::new(HashMap::new()),
        };

        let listener = Self(Arc::new(inner));
        let clone = listener.clone();

        tokio::spawn(async move {
            loop {
                match clone.Process().await {
                    Err(e) => {
                        error!("Listener fail with error {:?}, restart ...", e);
                        std::process::exit(0);
                    }
                    Ok(_) => break,
                }
            }
        });

        return Ok(listener);
    }

    pub async fn Watch(&self, predicate: Predicate) -> Result<Watcher> {
        let id = self
            .nextId
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let (watch, stream) = Watcher::New(self, id, predicate)?;

        let mut lock = self.watchers.lock().await;
        lock.insert(id, stream);

        return Ok(watch);
    }

    pub async fn RemoveWatch(&self, id: u64) -> Result<()> {
        let mut lock = self.watchers.lock().await;
        lock.remove(&id);

        return Ok(());
    }

    pub fn Close(&self) {
        self.closeNotify.notify_waiters();
    }

    pub async fn Process(&self) -> Result<()> {
        let mut listener = PgListener::connect_with(&self.pool.clone()).await?;
        listener.listen(&self.channel).await.unwrap();

        loop {
            tokio::select! {
                notification = listener.recv() => {
                    let notification = notification?;
                    let payload: serde_json::value::Value = serde_json::from_str(notification.payload())?;

                    let mut toRemove = Vec::new();
                    let mut ws = self.watchers.lock().await;
                    for (id, w) in ws.iter() {
                        match w.Send(&payload) {
                            Ok(()) => (),
                            Err(_) => {
                                toRemove.push(*id);
                            }
                        }
                    }

                    for id in toRemove {
                        ws.remove(&id);
                    }
                }
                _ = self.closeNotify.notified() => {
                    self.closed.store(false, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
            }
        }

        return Ok(());
    }
}
