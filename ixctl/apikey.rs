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

use inferxlib::common::*;

use crate::command::{ApikeyCreateRequest, ApikeyDeleteRequest, GlobalConfig};

#[derive(Debug)]
pub struct ApikeyCmd {
    pub verb: String,
    pub username: String,
    pub keyname: String,
    pub scope: Option<String>,
    pub restrict_tenant: Option<String>,
    pub restrict_namespace: Option<String>,
    pub expires_in_days: Option<u32>,
}

impl ApikeyCmd {
    pub fn Init(cmd_matches: &ArgMatches) -> Result<Self> {
        let expires_in_days = match cmd_matches.value_of("expires_in_days") {
            None => None,
            Some(v) => Some(v.parse::<u32>().map_err(|e| {
                Error::CommonError(format!("invalid expires-in-days '{}': {:?}", v, e))
            })?),
        };

        return Ok(Self {
            verb: cmd_matches.value_of("verb").unwrap().to_string(),
            keyname: cmd_matches.value_of("keyname").unwrap_or("").to_string(),
            username: cmd_matches
                .value_of("username_opt")
                .or_else(|| cmd_matches.value_of("username"))
                .unwrap_or("")
                .to_string(),
            scope: cmd_matches.value_of("scope").map(|s| s.to_string()),
            restrict_tenant: cmd_matches
                .value_of("restrict_tenant")
                .map(|s| s.to_string()),
            restrict_namespace: cmd_matches
                .value_of("restrict_namespace")
                .map(|s| s.to_string()),
            expires_in_days: expires_in_days,
        });
    }

    pub fn SubCommand<'a, 'b>() -> App<'a, 'b> {
        return SubCommand::with_name("apikey")
            .setting(AppSettings::ColoredHelp)
            .arg(
                Arg::with_name("verb")
                    .required(true)
                    .help("action name: create/delete/revoke/list")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("keyname")
                    .required(false)
                    .help("key name")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("username")
                    .required(false)
                    .help("user name (positional, legacy)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("username_opt")
                    .required(false)
                    .long("username")
                    .help("user name")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("scope")
                    .required(false)
                    .long("scope")
                    .help("apikey scope: full|inference|read")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("restrict_tenant")
                    .required(false)
                    .long("restrict-tenant")
                    .help("restrict apikey to tenant")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("restrict_namespace")
                    .required(false)
                    .long("restrict-namespace")
                    .help("restrict apikey to namespace (requires restrict-tenant)")
                    .takes_value(true),
            )
            .arg(
                Arg::with_name("expires_in_days")
                    .required(false)
                    .long("expires-in-days")
                    .help("optional key lifetime in days")
                    .takes_value(true),
            )
            .about("Manage API keys");
    }

    fn ValidateScope(&self) -> Result<()> {
        if let Some(scope) = &self.scope {
            match scope.as_str() {
                "full" | "inference" | "read" => (),
                _ => {
                    return Err(Error::CommonError(format!(
                        "invalid scope {} (expect full|inference|read)",
                        scope
                    )))
                }
            }
        }

        return Ok(());
    }

    fn ValidateCreateOptions(&self) -> Result<()> {
        self.ValidateScope()?;

        if self.keyname.len() == 0 {
            return Err(Error::CommonError(format!(
                "Apikey create must have keyname"
            )));
        }

        if self.restrict_namespace.is_some() && self.restrict_tenant.is_none() {
            return Err(Error::CommonError(format!(
                "restrict-namespace requires restrict-tenant"
            )));
        }

        if let Some(days) = self.expires_in_days {
            if days == 0 {
                return Err(Error::CommonError(format!(
                    "expires-in-days must be greater than 0"
                )));
            }
        }

        return Ok(());
    }

    fn RejectCreateOnlyOptions(&self, verb: &str) -> Result<()> {
        if self.scope.is_some()
            || self.restrict_tenant.is_some()
            || self.restrict_namespace.is_some()
            || self.expires_in_days.is_some()
        {
            return Err(Error::CommonError(format!(
                "apikey {} does not accept create-only flags (--scope, --restrict-tenant, --restrict-namespace, --expires-in-days)",
                verb
            )));
        }

        return Ok(());
    }

    pub async fn Run(&self, gConfig: &GlobalConfig) -> Result<()> {
        println!("ApikeyCmd is {:?}", self);

        let client = gConfig.GetObjectClient();

        match &self.verb as &str {
            "create" => {
                self.ValidateCreateOptions()?;
                let req = ApikeyCreateRequest {
                    username: self.username.clone(),
                    keyname: self.keyname.clone(),
                    scope: self.scope.clone(),
                    restrict_tenant: self.restrict_tenant.clone(),
                    restrict_namespace: self.restrict_namespace.clone(),
                    expires_in_days: self.expires_in_days,
                };

                match client.CreateApikey(&gConfig.accessToken, &req).await {
                    Err(e) => {
                        println!("can't create apikey with {:#?}", e);
                        return Ok(());
                    }
                    Ok(obj) => {
                        println!("{}", serde_json::to_string_pretty(&obj)?);
                        return Ok(());
                    }
                }
            }
            "delete" | "revoke" => {
                self.RejectCreateOnlyOptions(&self.verb)?;

                if self.keyname.len() == 0 {
                    return Err(Error::CommonError(format!(
                        "Apikey delete must have keyname"
                    )));
                }
                let req = ApikeyDeleteRequest {
                    username: self.username.clone(),
                    keyname: self.keyname.clone(),
                };
                match client.DeleteApikey(&gConfig.accessToken, &req).await {
                    Err(e) => {
                        println!("can't delete apikey with {:#?}", e);
                        return Ok(());
                    }
                    Ok(msg) => {
                        println!("{}", msg);
                        return Ok(());
                    }
                }
            }
            "list" => {
                self.RejectCreateOnlyOptions("list")?;

                if self.keyname.len() != 0 {
                    return Err(Error::CommonError(format!(
                        "Apikey list must have no keyname"
                    )));
                }

                if self.username.len() != 0 {
                    return Err(Error::CommonError(format!(
                        "Apikey list must have no username"
                    )));
                }

                match client.ListApikeys(&gConfig.accessToken).await {
                    Err(e) => {
                        println!("can't create apikey with {:#?}", e);
                        return Ok(());
                    }
                    Ok(obj) => {
                        println!("{}", serde_json::to_string_pretty(&obj)?);
                        return Ok(());
                    }
                }
            }
            _ => {
                return Err(Error::CommonError(format!(
                    "Apikey unknown verb {}",
                    &self.verb
                )));
            }
        }
    }
}
