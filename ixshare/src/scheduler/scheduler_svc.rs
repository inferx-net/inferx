// Copyright (c) 2025 InferX Authors / 2014 The Kubernetes Authors
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

use std::result::Result as SResult;
use tonic::transport::Server;

use crate::common::*;
use crate::na;

use super::scheduler::SCHEDULER;
use super::scheduler::SCHEDULER_CONFIG;

pub struct SchedulerSvc {}

#[tonic::async_trait]
impl na::scheduler_service_server::SchedulerService for SchedulerSvc {
    async fn lease_worker(
        &self,
        request: tonic::Request<na::LeaseWorkerReq>,
    ) -> SResult<tonic::Response<na::LeaseWorkerResp>, tonic::Status> {
        let msg: na::LeaseWorkerReq = request.into_inner();
        let resp = SCHEDULER.LeaseWorker(msg.clone()).await.unwrap();
        error!("lease_worker req {:?}/{}/{}", &msg, &resp.id, &resp.error);
        return Ok(tonic::Response::new(resp));
    }

    async fn return_worker(
        &self,
        request: tonic::Request<na::ReturnWorkerReq>,
    ) -> SResult<tonic::Response<na::ReturnWorkerResp>, tonic::Status> {
        let msg: na::ReturnWorkerReq = request.into_inner();

        error!("return_worker req {:?}", &msg);
        let resp = SCHEDULER.ReturnWorker(msg).await.unwrap();
        return Ok(tonic::Response::new(resp));
    }

    async fn refresh_gateway(
        &self,
        request: tonic::Request<na::RefreshGatewayReq>,
    ) -> SResult<tonic::Response<na::RefreshGatewayResp>, tonic::Status> {
        let msg = request.into_inner();
        let resp = SCHEDULER.RefreshGateway(msg).await.unwrap();
        return Ok(tonic::Response::new(resp));
    }
}

pub async fn RunSchedulerSvc() -> Result<()> {
    let svc = SchedulerSvc {};

    let svcAddr = format!("0.0.0.0:{}", SCHEDULER_CONFIG.schedulerPort);

    let svcfuture = Server::builder()
        .add_service(na::scheduler_service_server::SchedulerServiceServer::new(
            svc,
        ))
        .serve(svcAddr.parse().unwrap());
    svcfuture.await?;
    return Ok(());
}
