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

use clap::{App, AppSettings, Arg, ArgMatches, SubCommand};
use serde_json::Value;

use inferxlib::common::*;

use crate::command::GlobalConfig;

#[derive(Debug)]
pub struct GetCmd {
    pub objType: String,
    pub tenant: String,
    pub namespace: String,
    pub name: String,
}

#[derive(Debug)]
pub struct ObjectMeta {
    revision: i64,
    value: Value,
}

impl GetCmd {
    pub fn Init(cmd_matches: &ArgMatches) -> Result<Self> {
        return Ok(Self {
            objType: cmd_matches.value_of("type").unwrap().to_string(),
            tenant: cmd_matches.value_of("tenant").unwrap().to_string(),
            namespace: cmd_matches.value_of("namespace").unwrap().to_string(),
            name: cmd_matches.value_of("name").unwrap().to_string(),
        });
    }

    pub fn SubCommand<'a, 'b>() -> App<'a, 'b> {
        return SubCommand::with_name("get")
            .setting(AppSettings::ColoredHelp)
            .arg(
                Arg::with_name("type")
                    .required(true)
                    .help("object type")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("tenant")
                    .required(true)
                    .help("object tenant")
                    // .long("tenant")
                    // .short("t")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("namespace")
                    .required(true)
                    .help("object namespace")
                    // .long("namespace")
                    // .short("ns")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("name")
                    .required(true)
                    .help("object name")
                    .takes_value(true),
            )
            .about("get a object");
    }

    pub async fn Run(&self, gConfig: &GlobalConfig) -> Result<()> {
        let client = gConfig.GetObjectClient();
        let obj = match client
            .Get(&self.objType, &self.tenant, &self.namespace, &self.name)
            .await
        {
            Err(e) => {
                println!("doesn't find obj with {:#?}", e);
                return Ok(());
            }
            Ok(obj) => obj,
        };

        println!("{:#?}", obj);

        return Ok(());
    }
}
