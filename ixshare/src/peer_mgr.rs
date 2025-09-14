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

use core::ops::Deref;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::net::SocketAddrV4;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::net::TcpStream;

use crate::common::*;
use crate::node_config::NodeAgentConfig;
use crate::node_config::NODE_CONFIG;

pub const MASK_BITS: usize = 8;

lazy_static::lazy_static! {
    #[derive(Debug)]
    pub static ref NA_CONFIG: NodeAgentConfig = NodeAgentConfig::New(&NODE_CONFIG);

    pub static ref PEER_MGR: PeerMgr = {
        let cidrStr = NA_CONFIG.Cidr();
        let ipv4 = ipnetwork::Ipv4Network::from_str(&cidrStr).unwrap();
        //let localIp = local_ip_address::local_ip().unwrap();
        let pm = PeerMgr::New(ipv4.prefix() as _, ipv4.network());
        pm
    };
}

#[derive(Debug)]
pub struct PeerInner {
    pub hostIp: u32,
    pub port: u16,
    pub cidrAddr: u32,
}

#[derive(Debug, Clone)]
pub struct Peer(Arc<PeerInner>);

impl Peer {
    pub fn New(hostIp: u32, port: u16, cidrAddr: u32) -> Self {
        let inner = PeerInner {
            hostIp: hostIp,
            port: port,
            cidrAddr: cidrAddr,
        };

        return Self(Arc::new(inner));
    }
}

impl Deref for Peer {
    type Target = Arc<PeerInner>;

    fn deref(&self) -> &Arc<PeerInner> {
        &self.0
    }
}

#[derive(Debug)]
pub struct PeerMgrInner {
    // map cidrAddr --> Peer
    pub peers: HashMap<u32, Peer>,
    pub maskbits: usize,
    pub mask: u32,
    pub cidr: u32,
}

#[derive(Debug, Clone)]
pub struct PeerMgr(Arc<RwLock<PeerMgrInner>>);

impl Deref for PeerMgr {
    type Target = Arc<RwLock<PeerMgrInner>>;

    fn deref(&self) -> &Arc<RwLock<PeerMgrInner>> {
        &self.0
    }
}

impl PeerMgr {
    pub fn New(_maskbits: usize, cidr: Ipv4Addr) -> Self {
        let maskbits = MASK_BITS;
        let mask = !((1 << maskbits) - 1);
        let mut bytes = cidr.octets();

        // if the node cidr is 10.1.1.0/8, the peer cidr is 10.1.0.0/16
        bytes[2] = 0;
        let cidr = IpAddress::New(&bytes);
        let inner = PeerMgrInner {
            peers: HashMap::new(),
            maskbits: maskbits,
            mask: mask,
            cidr: cidr.0,
        };

        let mgr = Self(Arc::new(RwLock::new(inner)));
        return mgr;
    }

    pub fn BelongCidr(&self, ip: u32) -> bool {
        let cidrAddr = ip & 0xff000000; // self.read().unwrap().mask;
        if cidrAddr == self.read().unwrap().cidr {
            return true;
        }

        return false;
    }

    pub fn AddPeer(&self, hostIp: u32, port: u16, cidrAddr: u32) -> Result<()> {
        let peer = Peer::New(hostIp, port, cidrAddr);
        let mut inner = self.write().unwrap();
        if inner.peers.contains_key(&cidrAddr) {
            return Err(Error::Exist(format!(
                "PeerMgr::AddPeer get existing peer {:?}",
                peer
            )));
        }

        inner.peers.insert(cidrAddr, peer);
        return Ok(());
    }

    pub fn RemovePeer(&self, cidrAddr: u32) -> Result<()> {
        let mut inner = self.write().unwrap();
        match inner.peers.remove(&cidrAddr) {
            None => {
                return Err(Error::NotExist(format!(
                    "PeerMgr::RemovePeer peer {:?} not existing",
                    cidrAddr
                )))
            }
            Some(_peer) => return Ok(()),
        }
    }

    pub fn LookforPeer(&self, ip: u32) -> Result<Peer> {
        let inner = self.read().unwrap();
        let cidrAddr = ip & inner.mask;
        match inner.peers.get(&cidrAddr).cloned() {
            None => {
                return Err(Error::PeerNotExist(format!(
                    "PeerMgr::LookforPeer peer {:x} doesn't exist",
                    ip
                )))
            }
            Some(peer) => return Ok(peer.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IxTcpClient {
    pub hostIp: u32,
    pub hostPort: u16,

    pub tenant: String,
    pub namespace: String,
    pub dstIp: u32,
    pub dstPort: u16,
    pub srcIp: u32,
    pub srcPort: u16,
}

impl IxTcpClient {
    pub async fn Connect(&self) -> Result<TcpStream> {
        let ipv4Addr = IpAddress(self.hostIp).ToIpv4Addr();
        let socketv4Addr = SocketAddrV4::new(ipv4Addr, self.hostPort);

        let stream = match TcpStream::connect(socketv4Addr).await {
            // let stream = match socket.connect(socketv4Addr.into()).await {
            Err(e) => {
                error!(
                    "InferxTcpClient::Connect fail hostip {:?} hostport {} {:?}",
                    IpAddress(self.hostIp).AsBytes(),
                    self.hostPort,
                    &e
                );
                return Err(e.into());
            }
            Ok(s) => s,
        };

        let podNamespace = format!("{}/{}", self.tenant, self.namespace);

        let mut req = TsotConnReq {
            podNamespace: [0; 64],
            dstIp: self.dstIp,
            dstPort: self.dstPort,
            srcIp: self.srcIp,
            srcPort: self.srcPort,
        };

        for i in 0..podNamespace.as_bytes().len() {
            req.podNamespace[i] = podNamespace.as_bytes()[i];
        }

        self.WriteConnReq(&stream, req).await?;

        let resp = self.ReadConnResp(&stream).await?;

        if resp.errcode != TsotErrCode::Ok as u32 {
            return Err(Error::CommonError(format!(
                "TcpClientConnection connect fail with error {:?}",
                resp.errcode
            )));
        }

        return Ok(stream);
    }

    pub async fn WriteConnReq(&self, stream: &TcpStream, req: TsotConnReq) -> Result<()> {
        const REQ_SIZE: usize = std::mem::size_of::<TsotConnReq>();
        let addr = &req as *const _ as u64 as *const u8;
        let buf = unsafe { std::slice::from_raw_parts(addr, REQ_SIZE) };
        let mut offset = 0;

        while offset < REQ_SIZE {
            stream.writable().await?;
            let cnt = stream.try_write(&buf[offset..])?;
            offset += cnt;
        }

        return Ok(());
    }

    pub async fn ReadConnResp(&self, stream: &TcpStream) -> Result<TsotConnResp> {
        const RESP_SIZE: usize = std::mem::size_of::<TsotConnResp>();
        let mut readBuf = [0; RESP_SIZE];
        let mut offset = 0;

        while offset < RESP_SIZE {
            stream.readable().await?;
            let cnt = stream.try_read(&mut readBuf[offset..])?;
            offset += cnt;
        }

        let msg = unsafe { *(&readBuf[0] as *const _ as u64 as *const TsotConnResp) };

        return Ok(msg);
    }
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsotErrCode {
    Ok = 0,
    Reject = 1,
}

#[derive(Debug, Clone, Copy)]
pub struct TsotConnReq {
    pub podNamespace: [u8; 64],
    pub dstIp: u32,
    pub dstPort: u16,
    pub srcIp: u32,
    pub srcPort: u16,
}

impl TsotConnReq {
    pub fn GetNamespace(&self) -> Result<String> {
        for i in 0..self.podNamespace.len() {
            if self.podNamespace[i] == 0 {
                if i == 0 {
                    return Ok("Default".to_owned());
                }
                let str = std::str::from_utf8(&self.podNamespace[0..i])?;
                return Ok(str.to_owned());
            }
        }

        let str = std::str::from_utf8(&self.podNamespace)?;
        return Ok(str.to_owned());
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TsotConnResp {
    pub errcode: u32,
}
