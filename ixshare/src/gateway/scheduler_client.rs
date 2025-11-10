use std::collections::BTreeSet;
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
pub struct SchedulerClient {
    pub leasedWorkers: TMutex<BTreeSet<LeasedWorker>>,
    pub schedulerUrl: TMutex<Option<String>>,
    pub client:
        Option<na::scheduler_service_client::SchedulerServiceClient<tonic::transport::Channel>>,
}

impl SchedulerClient {
    pub async fn GetClient(
        &self,
    ) -> Result<na::scheduler_service_client::SchedulerServiceClient<tonic::transport::Channel>>
    {
        let url = self.schedulerUrl.lock().await.clone();
        match url {
            None => {
                return Err(Error::CommonError(format!(
                    "SchedulerClient::GetClient no valid scheduler"
                )))
            }
            Some(url) => {
                let schedClient: na::scheduler_service_client::SchedulerServiceClient<
                    tonic::transport::Channel,
                > = na::scheduler_service_client::SchedulerServiceClient::connect(url).await?;
                return Ok(schedClient);
            }
        }
    }

    pub async fn Connect(&self, schedulerUrl: &String) -> Result<()> {
        let mut schedClient: na::scheduler_service_client::SchedulerServiceClient<
            tonic::transport::Channel,
        > = na::scheduler_service_client::SchedulerServiceClient::connect(schedulerUrl.to_owned())
            .await?;
        let mut connReq = na::ConnectReq {
            gateway_id: GatewayId(),
            workers: Vec::new(),
        };

        {
            let lock = self.leasedWorkers.lock().await;
            for w in lock.iter() {
                let worker = na::WorkerId {
                    tenant: w.tenant.to_owned(),
                    namespace: w.namespace.to_owned(),
                    funcname: w.funcname.to_owned(),
                    fprevision: w.fprevision,
                    id: w.id.to_owned(),
                };
                connReq.workers.push(worker);
            }
        }

        let request = tonic::Request::new(connReq);
        let response = schedClient.connect_scheduler(request).await?;
        let resp = response.into_inner();
        if resp.error.len() != 0 {
            error!(
                "Gateway id: {} fail as connect to new scheduler fail with error {:?}, need restart to avoid double lease workers",
                GatewayId(),
                &resp.error
            );

            panic!(
                "Gateway id: {} fail as connect to new scheduler fail with error {:?}, need restart to avoid double lease workers",
                GatewayId(),
                &resp.error
            )
        }

        *self.schedulerUrl.lock().await = Some(schedulerUrl.to_owned());

        return Ok(());
    }

    pub async fn Disconnect(&self) {
        self.schedulerUrl.lock().await.take();
    }

    pub async fn LeaseWorker(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        fprevision: i64,
    ) -> Result<LeaseWorkerResp> {
        let mut client = self.GetClient().await?;

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
            self.leasedWorkers.lock().await.insert(LeasedWorker {
                tenant: tenant.to_owned(),
                namespace: namespace.to_owned(),
                funcname: funcname.to_owned(),
                fprevision: fprevision,
                id: resp.id.clone(),
            });

            return Ok(resp);
        }
        return Err(Error::CommonError(resp.error));
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
        let mut client = self.GetClient().await?;

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

        self.leasedWorkers.lock().await.remove(&LeasedWorker {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            fprevision: fprevision,
            id: id.to_owned(),
        });

        if resp.error.len() == 0 {
            return Ok(());
        }

        return Err(Error::CommonError(format!(
            "Return Worker fail with error {}",
            resp.error
        )));
    }

    pub async fn RefreshGateway(&self) -> Result<()> {
        let mut client = self.GetClient().await?;
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
