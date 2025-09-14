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

use inferxlib::{common::*, data_obj::DataObject};
use serde_json::Value;

use crate::command::GlobalConfig;

#[derive(Debug)]
pub struct UpdateCmd {
    pub filename: String,
}

impl UpdateCmd {
    pub fn Init(cmd_matches: &ArgMatches) -> Result<Self> {
        return Ok(Self {
            filename: cmd_matches.value_of("filename").unwrap().to_string(),
        });
    }

    pub fn SubCommand<'a, 'b>() -> App<'a, 'b> {
        return SubCommand::with_name("update")
            .setting(AppSettings::ColoredHelp)
            .arg(
                Arg::with_name("filename")
                    .required(true)
                    .help("file name")
                    .takes_value(true),
            )
            .about("Create a python function package");
    }

    pub async fn Run(&self, gConfig: &GlobalConfig) -> Result<()> {
        let client = gConfig.GetObjectClient();
        println!("UpdateCmd is {:?} server is {:?}", self, gConfig.gatewayUrl);

        let content = match std::fs::read_to_string(&self.filename) {
            Err(e) => {
                println!("Can't open file {} with error {:?}", &self.filename, e);
                return Ok(());
            }
            Ok(c) => c,
        };

        let o = match DataObject::<Value>::NewFromString(&content) {
            Err(e) => {
                println!(
                    "Can't parse file {} as Json with error {:?}",
                    &self.filename, e
                );
                return Ok(());
            }
            Ok(c) => c,
        };

        let version = client.Update(&gConfig.accessToken, o.clone()).await?;

        let obj = o.CopyWithRev(version, version);

        println!("{:#?}", obj);

        return Ok(());
    }
}
