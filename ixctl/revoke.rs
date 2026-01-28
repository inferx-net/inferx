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

use crate::command::{GlobalConfig, Grant};
use inferxlib::common::*;

#[derive(Debug)]
pub struct RevokeCmd {
    pub objType: String,
    pub tenant: String,
    pub namespace: String,
    pub role: String,
    pub username: String,
}

impl RevokeCmd {
    pub fn Init(cmd_matches: &ArgMatches) -> Result<Self> {
        return Ok(Self {
            objType: cmd_matches.value_of("type").unwrap().to_string(),
            tenant: cmd_matches.value_of("tenant").unwrap().to_string(),
            namespace: cmd_matches.value_of("namespace").unwrap_or("").to_string(),
            role: cmd_matches.value_of("role").unwrap().to_string(),
            username: cmd_matches.value_of("username").unwrap().to_string(),
        });
    }

    pub fn SubCommand<'a, 'b>() -> App<'a, 'b> {
        return SubCommand::with_name("revoke")
            .setting(AppSettings::ColoredHelp)
            .arg(
                Arg::with_name("type")
                    .required(true)
                    .help("object type")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("role")
                    .required(true)
                    .help("role name")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("username")
                    .required(true)
                    .help("user name")
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
                    .required(false)
                    .help("object namespace")
                    // .long("namespace")
                    // .short("ns")
                    .takes_value(true),
            )
            .about("grant role of object to user");
    }

    pub async fn Run(&self, gConfig: &GlobalConfig) -> Result<()> {
        println!("RevokeCmd is {:?}", self);

        let grant = match Grant::New(
            &self.objType,
            &self.tenant,
            &self.namespace,
            &self.role,
            &self.username,
        ) {
            Ok(g) => g,
            Err(Error::CommonError(s)) => {
                println!("fail to parse input: error {}", s);
                return Ok(());
            }
            _ => unreachable!(),
        };

        let client = gConfig.GetObjectClient();
        match client.Revoke(&gConfig.accessToken, &grant).await {
            Err(e) => {
                println!("revoke fail with {:#?}", e);
            }
            Ok(()) => {
                println!("revoke succesfully");
            }
        };

        return Ok(());
    }
}
