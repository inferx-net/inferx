use std::collections::BTreeSet;

use std::ops::Deref;
use tokio::sync::Mutex as TMutex;

use crate::na::{self, LeaseWorkerResp};

use super::http_gateway::GatewayId;
use crate::common::*;

lazy_static::lazy_static! {
    pub static ref SCHEDULER_CLIENT: SchedulerClient = SchedulerClient::default();
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LeasedWorker {
    pub tenant: String,
    pub namespace: String,
    pub funcname: String,
    pub fprevision: i64,
    pub id: String,
}

#[derive(Debug, Default)]
pub struct SchedulerClientInner {
    pub leasedWorkers: BTreeSet<LeasedWorker>,
    pub client:
        Option<na::scheduler_service_client::SchedulerServiceClient<tonic::transport::Channel>>,
}

#[derive(Debug, Default)]
pub struct SchedulerClient(TMutex<SchedulerClientInner>);

impl Deref for SchedulerClient {
    type Target = TMutex<SchedulerClientInner>;

    fn deref(&self) -> &TMutex<SchedulerClientInner> {
        &self.0
    }
}

impl SchedulerClient {
    pub async fn Connect(&self, schedulerUrl: &String) -> Result<()> {
        let schedClient: na::scheduler_service_client::SchedulerServiceClient<
            tonic::transport::Channel,
        > = na::scheduler_service_client::SchedulerServiceClient::connect(schedulerUrl.to_owned())
            .await?;
        self.lock().await.client = Some(schedClient);
        return Ok(());
    }

    pub async fn Disconnect(&self) {
        self.lock().await.client.take();
    }

    pub async fn LeaseWorker(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        fprevision: i64,
    ) -> Result<LeaseWorkerResp> {
        match self.lock().await.client.as_mut() {
            None => {
                return Err(Error::CommonError(format!(
                    "ReturnWorker fail as no valid scheduler"
                )))
            }
            Some(client) => {
                let req = na::LeaseWorkerReq {
                    tenant: tenant.to_owned(),
                    namespace: namespace.to_owned(),
                    funcname: funcname.to_owned(),
                    fprevision: fprevision,
                    gateway_id: GatewayId(),
                };

                let request = tonic::Request::new(req);
                let response = client.lease_worker(request).await?;
                let resp = response.into_inner();
                if resp.error.len() == 0 {
                    return Ok(resp);
                }

                return Err(Error::CommonError(resp.error));
            }
        }
    }

    pub async fn ReturnWorker(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        fprevision: i64,
        id: &str,
        failworker: bool,
    ) -> Result<()> {
        match self.lock().await.client.as_mut() {
            None => {
                return Err(Error::CommonError(format!(
                    "ReturnWorker fail as no valid scheduler"
                )))
            }
            Some(client) => {
                let req = na::ReturnWorkerReq {
                    tenant: tenant.to_owned(),
                    namespace: namespace.to_owned(),
                    funcname: funcname.to_owned(),
                    fprevision: fprevision,
                    id: id.to_owned(),
                    failworker: failworker,
                };

                let request = tonic::Request::new(req);
                let response = client.return_worker(request).await?;
                let resp = response.into_inner();
                if resp.error.len() == 0 {
                    return Ok(());
                }

                return Err(Error::CommonError(format!(
                    "Return Worker fail with error {}",
                    resp.error
                )));
            }
        }
    }

    pub async fn RefreshGateway(&self) -> Result<()> {
        match self.lock().await.client.as_mut() {
            None => {
                return Err(Error::CommonError(format!(
                    "ReturnWorker fail as no valid scheduler"
                )))
            }
            Some(client) => {
                let req = na::RefreshGatewayReq {
                    gateway_id: GatewayId(),
                };

                let request = tonic::Request::new(req);
                let response = client.refresh_gateway(request).await?;
                let resp = response.into_inner();
                if resp.error.len() == 0 {
                    return Ok(());
                }

                return Err(Error::CommonError(format!(
                    "ReturnGateway fail with error {}",
                    resp.error
                )));
            }
        }
    }
}
