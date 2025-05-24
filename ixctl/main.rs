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

#![allow(dead_code)]
#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

#[macro_use]
extern crate log;

#[macro_use]
extern crate clap;

pub mod command;
pub mod create;
// pub mod create_pypackage;
pub mod delete;
pub mod get;
pub mod list;
pub mod object_client;
pub mod update;

use oauth2::{
    basic::BasicClient, reqwest::async_http_client, AuthUrl, ClientId, ClientSecret,
    ResourceOwnerPassword, ResourceOwnerUsername, TokenResponse, TokenUrl,
};

use command::{Parse, Run};
use inferxlib::common::*;

pub struct ClientConfig {
    pub url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let accessToken = AuthN().await;
    let mut args = match Parse() {
        Ok(args) => args,
        Err(e) => {
            error!("the parse error is {:?}", e);
            panic!("exitting...")
        }
    };

    args.gConfig.accessToken = accessToken;

    Run(&mut args).await?;

    return Ok(());
}

pub struct Credential {
    pub secret: String,
    pub username: String,
    pub password: String,
}

pub enum Authorization {
    Credential(Credential),
    Apikey(String),
    None,
}

fn GetCred() -> Authorization {
    match std::env::var("IFERX_APIKEY") {
        Ok(s) => return Authorization::Apikey(s),
        Err(_) => (),
    };

    let secret = match std::env::var("IFERX_SECRET") {
        Ok(s) => s,
        Err(_) => return Authorization::None,
    };

    let username = match std::env::var("IFERX_USERNAME") {
        Ok(s) => s,
        Err(_) => return Authorization::None,
    };

    let password = match std::env::var("IFERX_PASSWORD") {
        Ok(s) => s,
        Err(_) => return Authorization::None,
    };

    let credential = Credential {
        secret: secret,
        username: username,
        password: password,
    };

    return Authorization::Credential(credential);
}

async fn AuthN() -> String {
    let keycloakUrl = match std::env::var("KEYCLOAK_URL") {
        Ok(s) => s,
        Err(_) => "http://localhost:31260/authn".to_owned(),
    };

    println!("keycloakUrl is {}", &keycloakUrl);
    match GetCred() {
        Authorization::None => return String::new(),
        Authorization::Apikey(k) => return k,
        Authorization::Credential(cred) => {
            let client = BasicClient::new(
                ClientId::new("infer_client".to_string()),
                Some(ClientSecret::new(cred.secret)),
                AuthUrl::new(format!(
                    "{}/realms/inferx/protocol/openid-connect/auth",
                    &keycloakUrl
                ))
                .unwrap(),
                Some(
                    TokenUrl::new(format!(
                        "{}/realms/inferx/protocol/openid-connect/token",
                        &keycloakUrl
                    ))
                    .unwrap(),
                ),
            );

            let token_result = client
                .exchange_password(
                    &ResourceOwnerUsername::new(cred.username),
                    &ResourceOwnerPassword::new(cred.password),
                )
                .request_async(async_http_client)
                .await
                .expect("Failed to obtain access token");

            println!("Access Token: {:?}", token_result.access_token().secret());
            return token_result.access_token().secret().clone();
        }
    }
}
