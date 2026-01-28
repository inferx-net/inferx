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

use crate::command::{GlobalConfig, UserRole};
use inferxlib::common::*;

#[derive(Debug)]
pub struct NamespaceUsersCmd {
    pub role: String,
    pub tenant: String,
    pub namespace: String,
}

impl NamespaceUsersCmd {
    pub fn Init(cmd_matches: &ArgMatches) -> Result<Self> {
        return Ok(Self {
            role: cmd_matches.value_of("role").unwrap().to_string(),
            tenant: cmd_matches.value_of("tenant").unwrap().to_string(),
            namespace: cmd_matches.value_of("namespace").unwrap().to_string(),
        });
    }

    pub fn SubCommand<'a, 'b>() -> App<'a, 'b> {
        return SubCommand::with_name("namespaceusers")
            .setting(AppSettings::ColoredHelp)
            .arg(
                Arg::with_name("role")
                    .required(true)
                    .help("role name")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("tenant")
                    .required(true)
                    .help("tenant name")
                    // .long("tenant")
                    // .short("t")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("namespace")
                    .required(true)
                    .help("namespace name")
                    // .long("namespace")
                    // .short("t")
                    .takes_value(true),
            )
            .about("get users with namespace role");
    }

    pub async fn Run(&self, gConfig: &GlobalConfig) -> Result<()> {
        println!("NamespaceUsersCmd is {:#?}", self);

        let role: &str = &self.role;
        let role = match role {
            "admin" => UserRole::Admin,
            "user" => UserRole::User,
            _ => return Err(Error::CommonError(format!("doesn't support role {}", role))),
        };

        let client = gConfig.GetObjectClient();
        let users = match client
            .NamespaceUsers(&gConfig.accessToken, &role, &self.tenant, &self.namespace)
            .await
        {
            Err(e) => {
                println!("doesn't find NamespaceUsers with {:#?}", e);
                return Ok(());
            }
            Ok(users) => users,
        };

        println!("{:#?}", users);

        return Ok(());
    }
}
