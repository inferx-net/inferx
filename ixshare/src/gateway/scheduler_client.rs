use crate::na::{self, LeaseWorkerResp};

use super::gw_obj_repo::SCHEDULER_URL;
use super::http_gateway::GatewayId;
use crate::common::*;

pub struct SchedulerClient {}

impl SchedulerClient {
    pub async fn LeaseWorker(
        &self,
        tenant: &str,
        namespace: &str,
        funcname: &str,
        fprevision: i64,
    ) -> Result<LeaseWorkerResp> {
        let schedulerUrl = match SCHEDULER_URL.lock().unwrap().clone() {
            None => {
                return Err(Error::CommonError(format!(
                    "AskFuncPod fail as no valid scheduler"
                )))
            }
            Some(u) => u,
        };
        let mut schedClient =
            na::scheduler_service_client::SchedulerServiceClient::connect(schedulerUrl).await?;

        let req = na::LeaseWorkerReq {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            fprevision: fprevision,
            gateway_id: GatewayId(),
        };

        let request = tonic::Request::new(req);
        let response = schedClient.lease_worker(request).await?;
        let resp = response.into_inner();
        if resp.error.len() == 0 {
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
        let schedulerUrl = match SCHEDULER_URL.lock().unwrap().clone() {
            None => {
                return Err(Error::CommonError(format!(
                    "ReturnWorker fail as no valid scheduler"
                )))
            }
            Some(u) => u,
        };
        let mut schedClient =
            na::scheduler_service_client::SchedulerServiceClient::connect(schedulerUrl).await?;

        let req = na::ReturnWorkerReq {
            tenant: tenant.to_owned(),
            namespace: namespace.to_owned(),
            funcname: funcname.to_owned(),
            fprevision: fprevision,
            id: id.to_owned(),
            failworker: failworker,
        };

        let request = tonic::Request::new(req);
        let response = schedClient.return_worker(request).await?;
        let resp = response.into_inner();
        if resp.error.len() == 0 {
            return Ok(());
        }

        return Err(Error::CommonError(format!(
            "Return Worker fail with error {}",
            resp.error
        )));
    }

    pub async fn RefreshGateway(&self) -> Result<()> {
        let schedulerUrl = match SCHEDULER_URL.lock().unwrap().clone() {
            None => {
                return Err(Error::CommonError(format!(
                    "ReturnWorker fail as no valid scheduler"
                )))
            }
            Some(u) => u,
        };

        let mut schedClient =
            na::scheduler_service_client::SchedulerServiceClient::connect(schedulerUrl).await?;

        let req = na::RefreshGatewayReq {
            gateway_id: GatewayId(),
        };

        let request = tonic::Request::new(req);
        let response = schedClient.refresh_gateway(request).await?;
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
