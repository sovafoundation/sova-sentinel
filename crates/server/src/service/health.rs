use sova_sentinel_proto::proto::{
    health_check_response::ServingStatus, health_server::Health, HealthCheckRequest,
    HealthCheckResponse,
};
use tonic::{Request, Response, Status};

#[derive(Default)]
pub struct HealthService;

#[tonic::async_trait]
impl Health for HealthService {
    async fn check(
        &self,
        _request: Request<HealthCheckRequest>,
    ) -> Result<Response<HealthCheckResponse>, Status> {
        Ok(Response::new(HealthCheckResponse {
            status: ServingStatus::Serving as i32,
        }))
    }
}
