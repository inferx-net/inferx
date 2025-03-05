// Copyright (c) 2021 Quark Container Authors
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

use crate::command::GlobalConfig;

use inferxlib::common::*;

#[derive(Debug)]
pub struct DeleteCmd {
    pub objType: String,
    pub tenant: String,
    pub namespace: String,
    pub name: String,
}

impl DeleteCmd {
    pub fn Init(cmd_matches: &ArgMatches) -> Result<Self> {
        return Ok(Self {
            objType: cmd_matches.value_of("objectType").unwrap().to_string(),
            tenant: cmd_matches.value_of("tenant").unwrap().to_string(),
            namespace: cmd_matches.value_of("namespace").unwrap().to_string(),
            name: cmd_matches.value_of("name").unwrap().to_string(),
        });
    }

    pub fn SubCommand<'a, 'b>() -> App<'a, 'b> {
        return SubCommand::with_name("delete")
            .setting(AppSettings::ColoredHelp)
            .arg(
                Arg::with_name("objectType")
                    .required(true)
                    .help("object type")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("tenant")
                    .required(true)
                    .help("tenant")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("namespace")
                    .required(true)
                    .help("namespace")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("name")
                    .required(true)
                    .help("object name")
                    .takes_value(true),
            )
            .about("Create a python function package");
    }

    pub async fn Run(&self, gConfig: &GlobalConfig) -> Result<()> {
        let client = gConfig.GetObjectClient();
        println!("Deletecmd is {:?}", self);

        let _version = client
            .Delete(&self.objType, &self.tenant, &self.namespace, &self.name)
            .await?;

        return Ok(());
    }
}
