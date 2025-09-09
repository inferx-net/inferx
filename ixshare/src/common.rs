// Copyright (c) 2025 InferX Authors /  
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

use etcd_client::Error as EtcdError;
use inferxlib::common::Error as InferxLibError;
use prost::DecodeError;
use prost::EncodeError;
use pyo3::exceptions::PyTypeError;
use pyo3::PyErr;
use serde_json::Error as SerdeJsonError;
use std::net::Ipv4Addr;
use std::num::ParseIntError;
use std::str::Utf8Error;
use std::string::FromUtf8Error;
use tokio::task::JoinError;
use tonic::Status as TonicStatus;

#[derive(Debug)]
pub enum Error {
    None,
    ProcessDone,
    NoPermission,
    SchedulerErr(String),
    BollardErr(bollard::errors::Error),
    SchedulerNoEnoughResource(String),
    SysError(i32),
    SocketClose,
    NotExist(String),
    PeerNotExist(String),
    Exist(String),
    MpscSendFull(String),
    CommonError(String),
    StdIOErr(std::io::Error),
    SerdeJsonError(SerdeJsonError),
    ReqWestErr(reqwest::Error),
    TonicStatus(TonicStatus),
    StdErr(Box<dyn std::error::Error>),
    TonicTransportErr(tonic::transport::Error),
    ParseIntError(ParseIntError),
    RegexError(regex::Error),
    Utf8Error(Utf8Error),
    FromUtf8Error(FromUtf8Error),
    AcquireError(tokio::sync::AcquireError),
    TokioChannFull,
    TokioChannClose,
    IpNetworkError(ipnetwork::IpNetworkError),
    EncodeError(EncodeError),
    DecodeError(DecodeError),
    Timeout,
    MinRevsionErr(MinRevsionErr),
    NewKeyExistsErr(NewKeyExistsErr),
    DeleteRevNotMatchErr(DeleteRevNotMatchErr),
    UpdateRevNotMatchErr(UpdateRevNotMatchErr),
    EtcdError(EtcdError),
    ContextCancel,
    JoinError(JoinError),
    AxumHttpError(axum::http::Error),
    AxumHttpUriError(axum::http::uri::InvalidUri),
    HyperError(hyper::Error),
    TokioOneshotError(tokio::sync::oneshot::error::RecvError),
    SqlxError(sqlx::Error),
    AddControlMessageError(tokio_seqpacket::ancillary::AddControlMessageError),
    SqliteErr(rusqlite::Error),
    KeycloakError(keycloak::KeycloakError),
}

unsafe impl core::marker::Send for Error {}

impl From<InferxLibError> for Error {
    fn from(item: InferxLibError) -> Self {
        match item {
            InferxLibError::CommonError(s) => return Self::CommonError(s),
            InferxLibError::NotExist(s) => return Self::NotExist(s),
            InferxLibError::Exist(s) => return Self::Exist(s),
            InferxLibError::SchedulerNoEnoughResource(s) => {
                return Self::SchedulerNoEnoughResource(s)
            }
            InferxLibError::SerdeJsonError(s) => return Self::SerdeJsonError(s),
            InferxLibError::StdIOErr(s) => return Self::StdIOErr(s),
            InferxLibError::ReqWestErr(s) => return Self::ReqWestErr(s),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(item: rusqlite::Error) -> Self {
        return Self::SqliteErr(item);
    }
}

impl From<keycloak::KeycloakError> for Error {
    fn from(item: keycloak::KeycloakError) -> Self {
        return Self::KeycloakError(item);
    }
}

impl From<tokio_seqpacket::ancillary::AddControlMessageError> for Error {
    fn from(item: tokio_seqpacket::ancillary::AddControlMessageError) -> Self {
        return Self::AddControlMessageError(item);
    }
}

impl From<bollard::errors::Error> for Error {
    fn from(item: bollard::errors::Error) -> Self {
        return Self::BollardErr(item);
    }
}

impl From<sqlx::Error> for Error {
    fn from(item: sqlx::Error) -> Self {
        return Self::SqlxError(item);
    }
}

impl From<tokio::sync::oneshot::error::RecvError> for Error {
    fn from(item: tokio::sync::oneshot::error::RecvError) -> Self {
        return Self::TokioOneshotError(item);
    }
}

impl From<hyper::Error> for Error {
    fn from(item: hyper::Error) -> Self {
        return Self::HyperError(item);
    }
}

impl From<axum::http::Error> for Error {
    fn from(item: axum::http::Error) -> Self {
        return Self::AxumHttpError(item);
    }
}

impl From<axum::http::uri::InvalidUri> for Error {
    fn from(item: axum::http::uri::InvalidUri) -> Self {
        return Self::AxumHttpUriError(item);
    }
}

impl From<JoinError> for Error {
    fn from(item: JoinError) -> Self {
        return Self::JoinError(item);
    }
}

impl From<EtcdError> for Error {
    fn from(item: EtcdError) -> Self {
        return Self::EtcdError(item);
    }
}

impl From<EncodeError> for Error {
    fn from(item: EncodeError) -> Self {
        return Self::EncodeError(item);
    }
}

impl From<DecodeError> for Error {
    fn from(item: DecodeError) -> Self {
        return Self::DecodeError(item);
    }
}

impl From<ipnetwork::IpNetworkError> for Error {
    fn from(item: ipnetwork::IpNetworkError) -> Self {
        return Self::IpNetworkError(item);
    }
}

impl From<tokio::sync::AcquireError> for Error {
    fn from(item: tokio::sync::AcquireError) -> Self {
        return Self::AcquireError(item);
    }
}

impl From<Utf8Error> for Error {
    fn from(item: Utf8Error) -> Self {
        return Self::Utf8Error(item);
    }
}

impl From<FromUtf8Error> for Error {
    fn from(item: FromUtf8Error) -> Self {
        return Self::FromUtf8Error(item);
    }
}

impl From<regex::Error> for Error {
    fn from(item: regex::Error) -> Self {
        return Self::RegexError(item);
    }
}

impl From<std::io::Error> for Error {
    fn from(item: std::io::Error) -> Self {
        return Self::StdIOErr(item);
    }
}

pub type Result<T> = core::result::Result<T, Error>;

impl From<SerdeJsonError> for Error {
    fn from(item: SerdeJsonError) -> Self {
        return Self::SerdeJsonError(item);
    }
}

impl From<reqwest::Error> for Error {
    fn from(item: reqwest::Error) -> Self {
        return Self::ReqWestErr(item);
    }
}

impl From<TonicStatus> for Error {
    fn from(item: TonicStatus) -> Self {
        return Self::TonicStatus(item);
    }
}

impl From<Box<dyn std::error::Error>> for Error {
    fn from(item: Box<dyn std::error::Error>) -> Self {
        return Self::StdErr(item);
    }
}

impl From<tonic::transport::Error> for Error {
    fn from(item: tonic::transport::Error) -> Self {
        return Self::TonicTransportErr(item);
    }
}

impl From<ParseIntError> for Error {
    fn from(item: ParseIntError) -> Self {
        return Self::ParseIntError(item);
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct IpAddress(pub u32);

impl IpAddress {
    pub fn AsBytes(&self) -> [u8; 4] {
        let mut bytes = [0u8; 4];
        bytes[0] = (self.0 >> 24) as u8;
        bytes[1] = (self.0 >> 16) as u8;
        bytes[2] = (self.0 >> 8) as u8;
        bytes[3] = (self.0 >> 0) as u8;
        return bytes;
    }

    pub fn New(bytes: &[u8; 4]) -> Self {
        let addr: u32 = ((bytes[0] as u32) << 24)
            | ((bytes[1] as u32) << 16)
            | ((bytes[2] as u32) << 8)
            | ((bytes[3] as u32) << 0);
        return Self(addr);
    }

    pub fn ToIpv4Addr(&self) -> Ipv4Addr {
        let bytes = self.AsBytes();
        return Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]);
    }
}

#[derive(Debug)]
pub struct MinRevsionErr {
    pub minRevision: i64,
    pub actualRevision: i64,
}

#[derive(Debug)]
pub struct NewKeyExistsErr {
    pub key: String,
    pub rv: i64,
}

#[derive(Debug)]
pub struct DeleteRevNotMatchErr {
    pub expectRv: i64,
    pub actualRv: i64,
}

#[derive(Debug)]
pub struct UpdateRevNotMatchErr {
    pub expectRv: i64,
    pub actualRv: i64,
}

impl Error {
    pub fn NewMinRevsionErr(minRevision: i64, actualRevision: i64) -> Self {
        return Self::MinRevsionErr(MinRevsionErr {
            minRevision: minRevision,
            actualRevision: actualRevision,
        });
    }

    pub fn NewNewKeyExistsErr(key: String, rv: i64) -> Self {
        return Self::NewKeyExistsErr(NewKeyExistsErr { key: key, rv: rv });
    }

    pub fn NewDeleteRevNotMatchErr(expectRv: i64, actualRv: i64) -> Self {
        return Self::DeleteRevNotMatchErr(DeleteRevNotMatchErr {
            expectRv: expectRv,
            actualRv: actualRv,
        });
    }

    pub fn NewUpdateRevNotMatchErr(expectRv: i64, actualRv: i64) -> Self {
        return Self::UpdateRevNotMatchErr(UpdateRevNotMatchErr {
            expectRv: expectRv,
            actualRv: actualRv,
        });
    }
}

impl Into<PyErr> for Error {
    fn into(self) -> PyErr {
        return PyErr::new::<PyTypeError, _>(format!("{:?}", self));
    }
}
