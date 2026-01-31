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

use crate::command::GlobalConfig;

#[derive(Debug)]
pub struct ApikeyCmd {
    pub verb: String,
    pub username: String,
    pub keyname: String,
}

impl ApikeyCmd {
    pub fn Init(cmd_matches: &ArgMatches) -> Result<Self> {
        return Ok(Self {
            verb: cmd_matches.value_of("verb").unwrap().to_string(),
            keyname: cmd_matches.value_of("keyname").unwrap_or("").to_string(),
            username: cmd_matches.value_of("username").unwrap_or("").to_string(),
        });
    }

    pub fn SubCommand<'a, 'b>() -> App<'a, 'b> {
        return SubCommand::with_name("apikey")
            .setting(AppSettings::ColoredHelp)
            .arg(
                Arg::with_name("verb")
                    .required(true)
                    .help("action name: create/delete/list")
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
                    .help("use name")
                    .takes_value(true),
            )
            .about("Create a api key");
    }

    pub async fn Run(&self, gConfig: &GlobalConfig) -> Result<()> {
        println!("ApikeyCmd is {:?}", self);

        let client = gConfig.GetObjectClient();

        match &self.verb as &str {
            "create" => {
                if self.keyname.len() == 0 {
                    return Err(Error::CommonError(format!(
                        "Apikey create must have keyname"
                    )));
                }
                match client
                    .CreateApikey(&gConfig.accessToken, &self.keyname, &self.username)
                    .await
                {
                    Err(e) => {
                        println!("can't create apikey with {:#?}", e);
                        return Ok(());
                    }
                    Ok(obj) => {
                        println!("create apikey  {:#?}", obj);
                        return Ok(());
                    }
                }
            }
            "delete" => {
                if self.keyname.len() == 0 {
                    return Err(Error::CommonError(format!(
                        "Apikey create must have keyname"
                    )));
                }
                match client
                    .DeleteApikey(&gConfig.accessToken, &self.keyname, &self.username)
                    .await
                {
                    Err(e) => {
                        println!("can't delete apikey with {:#?}", e);
                        return Ok(());
                    }
                    Ok(()) => {
                        return Ok(());
                    }
                }
            }
            "list" => {
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
                        println!("list apikeys  {:#?}", obj);
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
