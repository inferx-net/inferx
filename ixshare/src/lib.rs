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

#![allow(dead_code)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(deprecated)]
#![allow(unexpected_cfgs)]
#[macro_use]
extern crate scopeguard;

#[macro_use]
pub mod print;

extern crate alloc;

pub mod audit;
// pub mod cadvisor_types;
pub mod common;
pub mod consts;
pub mod databuf;
pub mod etcd;
pub mod eventlog;
pub mod gateway;
pub mod metastore;
pub mod node_config;
pub mod peer_mgr;
pub mod pgsql;
pub mod scheduler;
pub mod state_svc;
// pub mod tsot_msg;

pub mod tsot {
    include!("pb_gen/tsot.rs");
}

pub mod na {
    include!("pb_gen/na.rs");
}

pub mod ixmeta {
    include!("pb_gen/ixmeta.rs");
}

