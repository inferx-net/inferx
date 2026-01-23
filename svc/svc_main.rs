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
extern crate ixshare;

extern crate rand;

use std::io::Write;
use std::sync::Arc;

use ixshare::gateway::func_agent_mgr::GatewaySvc;
use ixshare::gateway::metrics::{InitTracer, GATEWAY_METRICS};
use ixshare::print::LOG;
use tokio::sync::Notify;

use ixshare::common::*;
use ixshare::metastore::unique_id::{UniqueId, UID};
use ixshare::node_config::NODE_CONFIG;
use ixshare::scheduler::scheduler::SchedulerSvc;

use ixshare::state_svc::state_svc::{StateService, STATESVC_CONFIG};

pub fn LogPanic(info: &str) {
    // std::fs::write("/opt/inferx/log/panic.log", info).expect("Unable to write file");

    error!("{}", info);
    let mut file = std::fs::OpenOptions::new()
        .create(true) // create if it doesn’t exist
        .append(true) // append instead of truncate
        .open("/opt/inferx/log/panic.log")
        .expect("Unable to write file");

    file.write_all(info.as_bytes())
        .expect("Unable to write file");
}

pub const RUN_SERVICE: &'static str = "RUN_SERVICE";

#[derive(Debug)]
pub enum RunService {
    StateSvc,
    Scheduler,
    Gateway,
    All,
}

async fn service_identifier() -> Result<String> {
    if let Ok(pod_ip) = std::env::var("POD_IP") {
        return Ok(pod_ip);
    }

    let id = UID.get().unwrap().Get().await?;
    return Ok(id.to_string());
}

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() -> Result<()> {
    std::panic::set_hook(Box::new(|info: &std::panic::PanicHookInfo<'_>| {
        let backtrace: backtrace::Backtrace = backtrace::Backtrace::new();
        if let Some(s) = info.payload().downcast_ref::<&str>() {
            eprintln!("Panic message: {}", s);
            let info = format!("Panic message: {}", s);
            LogPanic(&info);
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            eprintln!("Panic message: {}", s);
            let info = format!("Panic message: {}", s);
            LogPanic(&info);
        } else {
            eprintln!("Panic occurred but can't get the message.");
        }
        eprintln!("Panic occurred: {:?}", info);
        eprintln!("Backtrace:\n{:?}", backtrace);
        let info = format!("Panic occurred: {:?}", info);
        LogPanic(&info);
        let info = format!("Backtrace:\n{:?}", backtrace);
        LogPanic(&info);
        unsafe {
            libc::exit(1);
        }
    }));

    info!(
        "Start inferx service .... std::env::var(RUN_SERVICE) {:?}",
        std::env::var(RUN_SERVICE)
    );

    InitTracer().await;
    GATEWAY_METRICS.lock().await.Register().await;

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

    let Uid = UniqueId::New(&STATESVC_CONFIG.etcdAddrs).await?;
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
                res = SchedulerSvc() => {
                    info!("schedulerFuture finish {:?}", res);
                }
                res = GatewaySvc(Some(notify.clone())) => {
                    info!("Gateway finish {:?}", res);
                }
            }
        }
        RunService::Gateway => {
            LOG.SetServiceName("Gateway");
            info!("Gateway start ...");
            tokio::select! {
                res = GatewaySvc(None) => {
                    info!("Gateway finish {:?}", res);
                }
            }
        }
        RunService::StateSvc => {
            let svc_identifier = service_identifier().await?;
            let service_name = format!("StateSvc-{}", svc_identifier);
            LOG.SetServiceName(&service_name);
            info!("StateSvc {} start ...", service_name);
            tokio::select! {
                res = StateService(None) => {
                    info!("stateservice finish {:?}", res);
                }
            }
        }
        RunService::Scheduler => {
            let scheduler_identifier = service_identifier().await?;
            let service_name = format!("Scheduler-{}", scheduler_identifier);
            LOG.SetServiceName(&service_name);
            info!("Scheduler {} start ...", service_name);
            tokio::select! {
                res = SchedulerSvc() => {
                    info!("schedulerFuture finish {:?}", res);
                }
            }
        }
    }

    return Ok(());
}
