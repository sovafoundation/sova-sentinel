use anyhow::Result;
use dotenv::dotenv;
use sova_sentinel_proto::proto::health_server::HealthServer;
use sova_sentinel_server::{
    db::Database,
    proto::slot_lock_service_server::SlotLockServiceServer,
    service::{
        BitcoinCoreRpcClient, BitcoinRpcClient, BitcoinRpcService, ExternalRpcClient,
        HealthService, SlotLockServiceImpl,
    },
};
use std::{env, sync::Arc, time::Duration};
use tonic::transport::Server;
use tower::ServiceBuilder;
use tower_http::{
    classify::{GrpcCode, GrpcErrorsAsFailures, SharedClassifier},
    compression::CompressionLayer,
    trace::{DefaultMakeSpan, TraceLayer},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();
    // Load .env file if it exists
    dotenv().ok();

    // Get configuration from environment variables or use defaults
    let host = env::var("SOVA_SENTINEL_HOST").unwrap_or_else(|_| "[::1]".to_string());
    let port = env::var("SOVA_SENTINEL_PORT").unwrap_or_else(|_| "50051".to_string());
    let db_path = env::var("SOVA_SENTINEL_DB_PATH").unwrap_or_else(|_| "slot_locks.db".to_string());
    let btc_rpc_url =
        env::var("BITCOIN_RPC_URL").unwrap_or_else(|_| "http://localhost:18443".to_string());
    let btc_rpc_user = env::var("BITCOIN_RPC_USER").unwrap_or_else(|_| "user".to_string());
    let btc_rpc_pass = env::var("BITCOIN_RPC_PASS").unwrap_or_else(|_| "pass".to_string());
    let rpc_connection_type =
        env::var("BITCOIN_RPC_CONNECTION_TYPE").unwrap_or_else(|_| "bitcoincore".to_string());

    let btc_confirmation_threshold = env::var("BITCOIN_CONFIRMATION_THRESHOLD")
        .unwrap_or_else(|_| "6".to_string())
        .parse::<u32>()
        .map_err(|_| {
            anyhow::anyhow!("BITCOIN_CONFIRMATION_THRESHOLD must be a positive integer")
        })?;
    let btc_revert_threshold = env::var("BITCOIN_REVERT_THRESHOLD")
        .unwrap_or_else(|_| "18".to_string())
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("BITCOIN_REVERT_THRESHOLD must be a positive integer"))?;
    let btc_max_retries = env::var("BITCOIN_RPC_MAX_RETRIES")
        .unwrap_or_else(|_| "5".to_string())
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("BITCOIN_RPC_MAX_RETRIES must be a positive integer"))?;

    let addr = format!("{}:{}", host, port).parse()?;

    // Initialize database with thread-safe configuration
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_FULL_MUTEX,
    )?;

    let db = Database::new(conn)?;

    // Create Bitcoin service
    let rpc_client: Arc<dyn BitcoinRpcClient> = match rpc_connection_type.to_lowercase().as_str() {
        "bitcoincore" => Arc::new(BitcoinCoreRpcClient::new(
            btc_rpc_url.clone(),
            btc_rpc_user.clone(),
            btc_rpc_pass.clone(),
        )?),
        "external" => Arc::new(ExternalRpcClient::new(
            btc_rpc_url.clone(),
            btc_rpc_user.clone(),
            btc_rpc_pass.clone(),
        )),
        other => {
            return Err(format!("Unsupported rpc_connection_type: {}", other).into());
        }
    };

    let bitcoin_service =
        BitcoinRpcService::new(rpc_client, btc_confirmation_threshold, btc_max_retries);

    let service = SlotLockServiceImpl::new(db, bitcoin_service, btc_revert_threshold);

    tracing::info!("Database path: {}", db_path);
    tracing::info!("SlotLock server listening on {}", addr);

    // Response classifier that doesn't consider `Ok`, `Invalid Argument`, or `Not Found` as
    // failures
    let classifier = GrpcErrorsAsFailures::new()
        .with_success(GrpcCode::InvalidArgument)
        .with_success(GrpcCode::NotFound);

    let middleware = ServiceBuilder::new()
        .layer(CompressionLayer::new())
        .layer(
            TraceLayer::new(SharedClassifier::new(classifier))
                .make_span_with(DefaultMakeSpan::new().include_headers(true)),
        )
        .into_inner();

    Server::builder()
        .timeout(Duration::from_secs(20))
        .layer(middleware)
        .add_service(SlotLockServiceServer::new(service))
        .add_service(HealthServer::new(HealthService))
        .serve(addr)
        .await?;

    Ok(())
}
