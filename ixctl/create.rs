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

use base64::{engine::general_purpose, Engine as _};
use clap::{App, AppSettings, Arg, ArgMatches, SubCommand};

use crate::command::GlobalConfig;
use inferxlib::common::*;
use inferxlib::data_obj::DataObject;
use inferxlib::obj_mgr::func_mgr::Function;
use regex::Regex;
use serde_json::Value;

#[derive(Debug)]
pub struct CreateCmd {
    pub filename: String,
}

impl CreateCmd {
    pub fn Init(cmd_matches: &ArgMatches) -> Result<Self> {
        return Ok(Self {
            filename: cmd_matches.value_of("filename").unwrap().to_string(),
        });
    }

    pub fn SubCommand<'a, 'b>() -> App<'a, 'b> {
        return SubCommand::with_name("create")
            .setting(AppSettings::ColoredHelp)
            .arg(
                Arg::with_name("filename")
                    .required(true)
                    .help("file name")
                    .takes_value(true),
            )
            .about("Create a inferx object");
    }

    pub async fn Run(&self, gConfig: &GlobalConfig) -> Result<()> {
        println!("CreateCmd is {:?}", self);

        let content = match std::fs::read_to_string(&self.filename) {
            Err(e) => {
                println!("Can't open file {} with error {:?}", &self.filename, e);
                return Ok(());
            }
            Ok(c) => c,
        };

        let obj = match DataObject::<Value>::NewFromString(&content) {
            Err(e) => {
                println!(
                    "Can't parse file {} as Json with error {:?}",
                    &self.filename, e
                );
                return Ok(());
            }
            Ok(c) => c,
        };

        let obj = match obj.objType.as_str() {
            Function::KEY => match LoadAttachFiles(&obj) {
                Err(e) => {
                    println!("Can't load attached fileswith error {:?}", e);
                    return Ok(());
                }
                Ok(o) => o,
            },
            _ => obj,
        };

        println!("CreateCmd obj is {:#?}", &obj);
        let client = gConfig.GetObjectClient();
        let version = client.Create(&gConfig.accessToken, obj.clone()).await?;

        let obj = obj.CopyWithRev(version, version);

        println!("{:#?}", obj);

        return Ok(());
    }
}

pub fn LoadAttachFiles(obj: &DataObject<Value>) -> Result<DataObject<Value>> {
    let mut func = Function::FromDataObject(obj.clone())?;
    let mountfiles = &mut func.object.spec.mountfiles;
    let mut totalLen = 0;
    for f in mountfiles {
        if !is_path_safe(&f.targetpath) {
            return Err(Error::CommonError(format!(
                "targetpath {} is not valid",
                &f.targetpath
            )));
        }

        let path = std::path::Path::new(&f.srcpath);
        if !path.exists() {
            return Err(Error::CommonError(
                format!("File not found: {}", f.srcpath).into(),
            ));
        }

        if !path.is_file() {
            return Err(Error::CommonError(format!(
                "path: {} is not file",
                f.srcpath
            )));
        }

        let file_bytes = std::fs::read(path)?;

        let base64_string = general_purpose::STANDARD.encode(&file_bytes);
        totalLen += base64_string.len();
        f.data = base64_string;
    }

    // 1MB
    if totalLen > 1 << 20 {
        return Err(Error::CommonError(format!(
            "total file base64 size more than 1MB, size {}",
            totalLen
        )));
    }

    return Ok(func.DataObject());
}

fn is_path_safe(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }

    let re = Regex::new(r"^/[a-zA-Z0-9/_.-]*$").expect("Invalid Regex");

    if !re.is_match(path) || path.contains("//") {
        return false;
    }

    if path.ends_with("/") {
        return false;
    }

    return true;
}
