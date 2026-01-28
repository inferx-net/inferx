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

use clap::{App, AppSettings, Arg};
use std::{collections::BTreeSet, env};

use inferxlib::common::*;

use serde::Deserialize;
use serde::Serialize;

use crate::apikey::ApikeyCmd;
use crate::create::CreateCmd;
use crate::delete::DeleteCmd;
use crate::get::GetCmd;
use crate::grant::GrantCmd;
use crate::list::ListCmd;
use crate::namespaceusers::NamespaceUsersCmd;
use crate::object_client::ObjectClient;
use crate::revoke::RevokeCmd;
use crate::roles::RolesCmd;
use crate::tenantusers::TenantUsersCmd;
use crate::update::UpdateCmd;

lazy_static::lazy_static! {
    pub static ref SUPPORT_OBJ_TYPES : BTreeSet<String> = [
        "package".to_string(),
    ].iter().cloned().collect();
}

pub const INFX_GATEWAY_URL: &str = "INFX_GATEWAY_URL";

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ObjectType {
    Tenant,
    Namespace,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    User,
}

impl UserRole {
    pub fn String(&self) -> String {
        match self {
            Self::Admin => "admin".to_owned(),
            Self::User => "user".to_owned(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Apikey {
    pub apikey: String,
    pub username: String,
    pub keyname: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Grant {
    pub objType: ObjectType,
    pub tenant: String,
    pub namespace: String,
    pub name: String,
    pub role: UserRole,
    pub username: String,
}

impl Grant {
    pub fn New(
        objType: &str,
        tenant: &str,
        namespace: &str,
        role: &str,
        username: &str,
    ) -> Result<Self> {
        let objType = match objType {
            "tenant" => ObjectType::Tenant,
            "namespace" => ObjectType::Namespace,
            _ => {
                return Err(Error::CommonError(format!(
                    "doesn't support object type {}",
                    objType
                )))
            }
        };

        let role = match role {
            "admin" => UserRole::Admin,
            "user" => UserRole::User,
            _ => return Err(Error::CommonError(format!("doesn't support role {}", role))),
        };

        match objType {
            ObjectType::Tenant => {
                return Ok(Self {
                    objType: objType,
                    tenant: "system".to_owned(),
                    namespace: "system".to_owned(),
                    name: tenant.to_owned(),
                    role: role,
                    username: username.to_owned(),
                });
            }
            ObjectType::Namespace => {
                return Ok(Self {
                    objType: objType,
                    tenant: tenant.to_owned(),
                    namespace: "system".to_owned(),
                    name: namespace.to_owned(),
                    role: role,
                    username: username.to_owned(),
                });
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RoleBinding {
    pub objType: ObjectType,
    pub role: UserRole,
    pub tenant: String,
    pub namespace: String,
}

pub struct GlobalConfig {
    pub gatewayUrl: String,
    pub accessToken: String,
}

impl GlobalConfig {
    pub fn GetObjectClient(&self) -> ObjectClient {
        let client = ObjectClient::New(&self.gatewayUrl);
        return client;
    }
}

pub struct Arguments {
    pub gConfig: GlobalConfig,
    pub cmd: Command,
}

#[derive(Debug)]
pub enum Command {
    Create(CreateCmd),
    List(ListCmd),
    Get(GetCmd),
    Delete(DeleteCmd),
    Update(UpdateCmd),
    Grant(GrantCmd),
    Revoke(RevokeCmd),
    TenantUsers(TenantUsersCmd),
    NamespaceUsers(NamespaceUsersCmd),
    Roles(RolesCmd),
    Apikey(ApikeyCmd),
}

pub async fn Run(args: &mut Arguments) -> Result<()> {
    match &mut args.cmd {
        Command::Create(cmd) => return cmd.Run(&args.gConfig).await,
        Command::List(cmd) => return cmd.Run(&args.gConfig).await,
        Command::Get(cmd) => return cmd.Run(&args.gConfig).await,
        Command::Delete(cmd) => return cmd.Run(&args.gConfig).await,
        Command::Update(cmd) => return cmd.Run(&args.gConfig).await,
        Command::Grant(cmd) => return cmd.Run(&args.gConfig).await,
        Command::Revoke(cmd) => return cmd.Run(&args.gConfig).await,
        Command::TenantUsers(cmd) => return cmd.Run(&args.gConfig).await,
        Command::NamespaceUsers(cmd) => return cmd.Run(&args.gConfig).await,
        Command::Roles(cmd) => return cmd.Run(&args.gConfig).await,
        Command::Apikey(cmd) => return cmd.Run(&args.gConfig).await,
    }
}

fn get_args() -> Vec<String> {
    return env::args().collect();
}

pub fn Parse() -> Result<Arguments> {
    let matches = App::new("ixctl")
        .about("ixctl - inferx client command line tool")
        .setting(AppSettings::ColoredHelp)
        .author("InferX Team")
        .setting(AppSettings::SubcommandRequired)
        .version(crate_version!())
        .arg(
            Arg::with_name("server")
                .help("gateway server url")
                .long("server")
                .short("s")
                .takes_value(true),
        )
        .subcommand(CreateCmd::SubCommand())
        .subcommand(ListCmd::SubCommand())
        .subcommand(GetCmd::SubCommand())
        .subcommand(DeleteCmd::SubCommand())
        .subcommand(UpdateCmd::SubCommand())
        .subcommand(GrantCmd::SubCommand())
        .subcommand(RevokeCmd::SubCommand())
        .subcommand(TenantUsersCmd::SubCommand())
        .subcommand(NamespaceUsersCmd::SubCommand())
        .subcommand(RolesCmd::SubCommand())
        .subcommand(ApikeyCmd::SubCommand())
        .get_matches_from(get_args());

    let gatewayUrl = match matches.value_of("server") {
        None => match std::env::var(INFX_GATEWAY_URL) {
            Ok(s) => s,
            Err(_e) => {
                panic!(
                    "can't get gateway url from commandline or Environment Variable {}",
                    INFX_GATEWAY_URL
                );
            }
        },
        Some(s) => s.to_owned(),
    };

    let gConfig = GlobalConfig {
        gatewayUrl: gatewayUrl,
        accessToken: "".to_owned(),
    };

    let args = match matches.subcommand() {
        ("create", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::Create(CreateCmd::Init(&cmd_matches)?),
        },
        ("get", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::Get(GetCmd::Init(&cmd_matches)?),
        },
        ("list", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::List(ListCmd::Init(&cmd_matches)?),
        },
        ("delete", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::Delete(DeleteCmd::Init(&cmd_matches)?),
        },
        ("update", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::Update(UpdateCmd::Init(&cmd_matches)?),
        },
        ("grant", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::Grant(GrantCmd::Init(&cmd_matches)?),
        },
        ("revoke", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::Revoke(RevokeCmd::Init(&cmd_matches)?),
        },
        ("tenantusers", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::TenantUsers(TenantUsersCmd::Init(&cmd_matches)?),
        },
        ("namespaceusers", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::NamespaceUsers(NamespaceUsersCmd::Init(&cmd_matches)?),
        },
        ("roles", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::Roles(RolesCmd::Init(&cmd_matches)?),
        },
        ("apikey", Some(cmd_matches)) => Arguments {
            gConfig: gConfig,
            cmd: Command::Apikey(ApikeyCmd::Init(&cmd_matches)?),
        },
        // We should never reach here because clap already enforces this
        x => panic!("command not recognized {:?}", x),
    };

    return Ok(args);
}
