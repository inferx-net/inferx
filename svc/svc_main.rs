// Copyright (c) 2025 InferX Authors /  
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
// limitations under

#![allow(dead_code)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(deprecated)]

#[macro_use]
extern crate log;
extern crate simple_logging;

extern crate rand;

use std::sync::Arc;

use ixshare::gateway::func_agent_mgr::GatewaySvc;
use ixshare::print::LOG;
use tokio::sync::Notify;

use ixshare::common::*;
use ixshare::metastore::unique_id::{UniqueId, UID};
use ixshare::node_config::NODE_CONFIG;
use ixshare::scheduler::scheduler::ExecSchedulerSvc;

use ixshare::state_svc::state_svc::StateService;

pub fn LogPanic(info: &str) {
    std::fs::write("/opt/inferx/log/panic.log", info).expect("Unable to write file");
    error!("{}:{}", LOG.ServiceName(), info);
}

pub const RUN_SERVICE: &'static str = "RUN_SERVICE";

#[derive(Debug)]
pub enum RunService {
    StateSvc,
    Scheduler,
    Gateway,
    All,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() -> Result<()> {
    std::panic::set_hook(Box::new(|info: &std::panic::PanicHookInfo<'_>| {
        let backtrace = backtrace::Backtrace::new();
        if let Some(s) = info.payload().downcast_ref::<&str>() {
            eprintln!("Panic message: {}", s);
            error!("Panic message: {}", s);
            let info = format!("Panic message: {}", s);
            LogPanic(&info);
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            eprintln!("Panic message: {}", s);
            error!("Panic message: {}", s);
            let info = format!("Panic message: {}", s);
            LogPanic(&info);
        } else {
            eprintln!("Panic occurred but can't get the message.");
            error!("Panic occurred but can't get the message.");
        }
        error!("Panic occurred: {:?}", info);
        eprintln!("Panic occurred: {:?}", info);
        error!("Backtrace:\n{:?}", backtrace);
        let info = format!("Panic occurred: {:?}", info);
        LogPanic(&info);
        let info = format!("Backtrace:\n{:?}", backtrace);
        LogPanic(&info);
        unsafe {
            libc::exit(1);
        }
    }));

    // log4rs::init_file(
    //     "/opt/inferx/config/onenode_logging_config.yaml",
    //     Default::default(),
    // )
    // .unwrap();

    error!(
        "Start inferx service ....xxx std::env::var(RUN_SERVICE) {:?}",
        std::env::var(RUN_SERVICE)
    );

    let runService = match std::env::var(RUN_SERVICE) {
        Err(_) => {
            if NODE_CONFIG.runService {
                RunService::All
            } else {
                RunService::StateSvc
            }
        }
        Ok(runsvc) => match runsvc.as_str() {
            "StateSvc" => RunService::StateSvc,
            "Scheduler" => RunService::Scheduler,
            "Gateway" => RunService::Gateway,
            "All" => RunService::All,
            _ => {
                error!(
                    "get invalid environment variable {} -> {}",
                    RUN_SERVICE, runsvc
                );
                panic!();
            }
        },
    };

    let Uid = UniqueId::New(&NODE_CONFIG.etcdAddrs).await?;
    UID.set(Uid).unwrap();

    match runService {
        RunService::All => {
            LOG.SetServiceName("onenode");
            error!("Onenode start ...");

            let notify = Arc::new(Notify::new());
            tokio::select! {
                res = StateService(Some(notify.clone())) => {
                    info!("stateservice finish {:?}", res);
                }
                res = Scheduler() => {
                    info!("schedulerFuture finish {:?}", res);
                }
                res = GatewaySvc(Some(notify.clone())) => {
                    info!("Gateway finish {:?}", res);
                }
            }
        }
        RunService::Gateway => {
            LOG.SetServiceName("Gateway");
            error!("Gateway start ...");
            tokio::select! {
                res = GatewaySvc(None) => {
                    info!("Gateway finish {:?}", res);
                }
            }
        }
        RunService::StateSvc => {
            LOG.SetServiceName("StateSvc");
            error!("StateSvc start ...");
            tokio::select! {
                res = StateService(None) => {
                    info!("stateservice finish {:?}", res);
                }
            }
        }
        RunService::Scheduler => {
            LOG.SetServiceName("Scheduler");
            error!("Scheduler start ...");
            tokio::select! {
                res = Scheduler() => {
                    info!("schedulerFuture finish {:?}", res);
                }
            }
        }
    }

    return Ok(());
}

async fn Scheduler() -> Result<()> {
    return ExecSchedulerSvc().await;
}

pub struct Scheduler {}

impl Scheduler {
    // need one more pod for the funcpackage to service request
    pub fn ScaleOut(&self, _fpKey: &str) -> Result<()> {
        unimplemented!()
    }

    // ok to scale in one pod
    pub fn ScaleIn(&self, _fpKey: &str) -> Result<()> {
        unimplemented!()
    }

    pub fn DemissionPod(&self, _podid: &str) -> Result<()> {
        unimplemented!()
    }
}
