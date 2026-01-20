use anyhow::Result;
use async_trait::async_trait;
use bitcoin::Txid;
use bitcoincore_rpc::{jsonrpc, Auth, Client, Error, RpcApi};
use reqwest::Client as HttpClient;
use serde_json::json;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio_retry::{
    strategy::{jitter, ExponentialBackoff},
    Retry,
};

#[derive(Error, Debug)]
pub enum BitcoinRpcError {
    #[error("Bitcoin node is unreachable after {attempts} attempts")]
    BitcoinNodeUnreachable { attempts: u32 },
}

#[async_trait]
pub trait BitcoinRpcClient: Send + Sync {
    async fn get_raw_transaction_info(
        &self,
        txid: &Txid,
    ) -> Result<bitcoincore_rpc::json::GetRawTransactionResult, Error>;
}

pub struct BitcoinCoreRpcClient {
    client: Arc<Client>,
}

impl BitcoinCoreRpcClient {
    pub fn new(
        url: String,
        user: String,
        password: String,
    ) -> Result<Self, bitcoincore_rpc::Error> {
        let auth = if user.is_empty() && password.is_empty() {
            Auth::None
        } else {
            Auth::UserPass(user, password)
        };
        let client = Client::new(&url, auth)?;
        Ok(Self {
            client: Arc::new(client),
        })
    }
}

#[async_trait]
impl BitcoinRpcClient for BitcoinCoreRpcClient {
    async fn get_raw_transaction_info(
        &self,
        txid: &Txid,
    ) -> Result<bitcoincore_rpc::json::GetRawTransactionResult, Error> {
        self.client.get_raw_transaction_info(txid, None)
    }
}

/// RPC client backed by an external HTTP service
pub struct ExternalRpcClient {
    client: HttpClient,
    url: String,
    auth: Option<(String, String)>,
}

impl ExternalRpcClient {
    pub fn new(url: String, user: String, password: String) -> Self {
        let auth = if user.is_empty() && password.is_empty() {
            None
        } else {
            Some((user, password))
        };
        Self {
            client: HttpClient::new(),
            url,
            auth,
        }
    }

    async fn make_rpc_call(
        &self,
        method: &str,
        params: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, Error> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let mut req = self.client.post(&self.url).json(&payload);
        if let Some((user, pass)) = &self.auth {
            req = req.basic_auth(user, Some(pass));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| Error::JsonRpc(jsonrpc::error::Error::Transport(Box::new(e))))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::JsonRpc(jsonrpc::error::Error::Transport(Box::new(e))))?;

        if let Some(err) = json.get("error") {
            if !err.is_null() {
                let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
                let message = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("RPC error")
                    .to_string();
                return Err(Error::JsonRpc(jsonrpc::error::Error::Rpc(
                    jsonrpc::error::RpcError {
                        code: code.try_into().unwrap(),
                        message,
                        data: None,
                    },
                )));
            }
        }

        json.get("result").cloned().ok_or_else(|| {
            Error::JsonRpc(jsonrpc::error::Error::Rpc(jsonrpc::error::RpcError {
                code: -32603,
                message: "missing result".into(),
                data: None,
            }))
        })
    }
}

#[async_trait]
impl BitcoinRpcClient for ExternalRpcClient {
    async fn get_raw_transaction_info(
        &self,
        txid: &Txid,
    ) -> Result<bitcoincore_rpc::json::GetRawTransactionResult, Error> {
        let res = self
            .make_rpc_call(
                "getrawtransaction",
                vec![json!(txid.to_string()), json!(true)],
            )
            .await?;
        serde_json::from_value(res)
            .map_err(|e| Error::JsonRpc(jsonrpc::error::Error::Transport(Box::new(e))))
    }
}

#[tonic::async_trait]
pub trait BitcoinRpcServiceAPI: Send + Sync {
    /// Checks if a transaction has enough confirmations
    /// Returns Ok(true) if confirmed, Ok(false) if not confirmed enough, and Err if transaction not found or other error
    async fn is_tx_confirmed(&self, txid: &str) -> Result<bool>;
}

type BitcoinRpcOperation<T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send>>;

#[derive(Clone)]
pub struct BitcoinRpcService {
    client: Arc<dyn BitcoinRpcClient>,
    confirmation_threshold: u32,
    max_retries: u32,
    base_delay: Duration,
}

impl BitcoinRpcService {
    /// Creates a new BitcoinRpcService instance
    pub fn new(
        client: Arc<dyn BitcoinRpcClient>,
        confirmation_threshold: u32,
        max_retries: u32,
    ) -> Self {
        Self {
            client,
            confirmation_threshold,
            max_retries,
            base_delay: Duration::from_millis(100),
        }
    }

    /// Creates a new BitcoinRpcService instance with a custom base delay
    pub fn with_base_delay(
        client: Arc<dyn BitcoinRpcClient>,
        confirmation_threshold: u32,
        max_retries: u32,
        base_delay: Duration,
    ) -> Self {
        Self {
            client,
            confirmation_threshold,
            max_retries,
            base_delay,
        }
    }

    /// Returns the current confirmation threshold
    pub fn confirmation_threshold(&self) -> u32 {
        self.confirmation_threshold
    }

    async fn with_retry<T>(
        &self,
        operation: impl Fn() -> BitcoinRpcOperation<T> + Send + Sync,
    ) -> Result<T>
    where
        T: Send,
    {
        let strategy = ExponentialBackoff::from_millis(self.base_delay.as_millis() as u64)
            .map(jitter)
            .take((self.max_retries - 1) as usize);

        let result = Retry::spawn(strategy, || {
            let operation = operation();
            async move {
                match operation.await {
                    Ok(result) => Ok(Ok(result)),
                    Err(e) => {
                        if Self::is_connectivity_error(&e) {
                            Err(e)
                        } else {
                            // For non-connectivity errors, return Ok to stop retrying
                            Ok(Err(e))
                        }
                    }
                }
            }
        })
        .await;

        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(anyhow::anyhow!("Operation failed: {}", e)),
            Err(_e) => Err(BitcoinRpcError::BitcoinNodeUnreachable {
                attempts: self.max_retries,
            }
            .into()),
        }
    }

    fn is_connectivity_error(error: &Error) -> bool {
        matches!(error, Error::JsonRpc(jsonrpc::error::Error::Transport(_)))
    }
}

#[tonic::async_trait]
impl BitcoinRpcServiceAPI for BitcoinRpcService {
    async fn is_tx_confirmed(&self, txid: &str) -> Result<bool> {
        let txid =
            Txid::from_str(txid).map_err(|e| anyhow::anyhow!("Invalid transaction ID: {}", e))?;

        let result = self
            .with_retry(|| {
                let client = self.client.clone();
                let threshold = self.confirmation_threshold;
                Box::pin(async move {
                    match client.get_raw_transaction_info(&txid).await {
                        Ok(tx_info) => match tx_info.confirmations {
                            Some(confirmations) => Ok(confirmations >= threshold),
                            None => Ok(false),
                        },
                        Err(Error::JsonRpc(jsonrpc::error::Error::Rpc(ref rpcerr)))
                            if rpcerr.code == -5 =>
                        {
                            // Error code -5 means transaction not found
                            Ok(false)
                        }
                        Err(e) => Err(e),
                    }
                })
            })
            .await?;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::{hashes::Hash, Txid, Wtxid};
    use std::sync::Mutex;

    struct MockBitcoinRpcClient {
        raw_transaction_info_config:
            Mutex<Option<MockCallConfig<bitcoincore_rpc::json::GetRawTransactionResult>>>,
    }

    struct MockCallConfig<T> {
        succeed_at_attempt: Option<usize>,
        error_fn: Box<dyn Fn() -> Error + Send + Sync>,
        success_response: T,
        attempts: Mutex<usize>,
    }

    impl<T: Clone> MockCallConfig<T> {
        fn new(
            error_fn: impl Fn() -> Error + 'static + Send + Sync,
            success_response: T,
            succeed_at_attempt: Option<usize>,
        ) -> Self {
            Self {
                succeed_at_attempt,
                error_fn: Box::new(error_fn),
                success_response,
                attempts: Mutex::new(0),
            }
        }
    }

    impl MockBitcoinRpcClient {
        fn new() -> Self {
            Self {
                raw_transaction_info_config: Mutex::new(None),
            }
        }

        // Configures mock behavior for get_raw_transaction_info with customized success/failure patterns
        fn setup_get_raw_transaction_info(
            &self,
            error_fn: impl Fn() -> Error + 'static + Send + Sync,
            success_response: bitcoincore_rpc::json::GetRawTransactionResult,
            succeed_at_attempt: Option<usize>,
        ) -> &Self {
            *self.raw_transaction_info_config.lock().unwrap() = Some(MockCallConfig::new(
                error_fn,
                success_response,
                succeed_at_attempt,
            ));
            self
        }

        // Helper to create a connection refused error
        fn create_connection_refused_error() -> Error {
            Error::JsonRpc(jsonrpc::error::Error::Transport(Box::new(
                std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused"),
            )))
        }

        // Helper to create a default transaction result
        fn create_default_tx_result() -> bitcoincore_rpc::json::GetRawTransactionResult {
            bitcoincore_rpc::json::GetRawTransactionResult {
                txid: Txid::all_zeros(),
                hash: Wtxid::all_zeros(),
                confirmations: Some(6),
                blockhash: None,
                in_active_chain: None,
                blocktime: None,
                time: None,
                version: 2,
                size: 0,
                vsize: 0,
                locktime: 0,
                vin: vec![],
                vout: vec![],
                hex: vec![],
            }
        }

        // Helper to set up a mock with a connectivity error
        fn setup_with_connectivity_error(&self, succeed_at: Option<usize>) -> &Self {
            self.setup_get_raw_transaction_info(
                Self::create_connection_refused_error,
                Self::create_default_tx_result(),
                succeed_at,
            )
        }
    }

    #[async_trait]
    impl BitcoinRpcClient for MockBitcoinRpcClient {
        async fn get_raw_transaction_info(
            &self,
            _txid: &Txid,
        ) -> Result<bitcoincore_rpc::json::GetRawTransactionResult, Error> {
            let config = self.raw_transaction_info_config.lock().unwrap();
            match config.as_ref() {
                Some(config) => {
                    let attempt = *config.attempts.lock().unwrap();
                    let result = match config.succeed_at_attempt {
                        Some(success_attempt) if attempt == success_attempt => {
                            Ok(config.success_response.clone())
                        }
                        _ => Err((config.error_fn)()),
                    };
                    *config.attempts.lock().unwrap() += 1;
                    result
                }
                None => Err(Error::JsonRpc(jsonrpc::error::Error::Transport(Box::new(
                    std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "Connection refused",
                    ),
                )))),
            }
        }
    }

    // Helper function to create a test service
    fn create_test_service(
        mock_client: Arc<dyn BitcoinRpcClient>,
        max_retries: u32,
    ) -> BitcoinRpcService {
        BitcoinRpcService::with_base_delay(
            mock_client,
            3, // Default confirmation threshold
            max_retries,
            Duration::from_millis(1), // Minimal delay for faster tests
        )
    }

    #[tokio::test]
    async fn test_tx_confirmed_retry_scenarios() {
        // Test cases: (succeed_at_attempt, max_retries, should_succeed)
        let test_cases = [
            (Some(0), 5, true),  // Succeed on first attempt
            (Some(2), 5, true),  // Succeed on third attempt
            (Some(4), 5, true),  // Succeed on fifth attempt (last retry)
            (Some(5), 5, false), // Would succeed on sixth attempt, but max is 5
            (None, 5, false),    // Never succeed
        ];

        for (case_idx, (succeed_at, max_retries, should_succeed)) in test_cases.iter().enumerate() {
            println!(
                "Running test case {}: succeed_at={:?}, max_retries={}, should_succeed={}",
                case_idx, succeed_at, max_retries, should_succeed
            );

            let mock_client = Arc::new(MockBitcoinRpcClient::new());
            mock_client.setup_with_connectivity_error(*succeed_at);

            let service = create_test_service(mock_client, *max_retries);

            let result = service
                .is_tx_confirmed("0000000000000000000000000000000000000000000000000000000000000000")
                .await;

            if *should_succeed {
                assert!(
                    result.is_ok(),
                    "Case {} should succeed but got error: {:?}",
                    case_idx,
                    result.err()
                );
            } else {
                assert!(
                    result.is_err(),
                    "Case {} should fail but succeeded",
                    case_idx
                );

                // Additional check for the expected error type
                if let Err(err) = result {
                    let err_debug = format!("{:?}", err);
                    let bitcoin_err = err.downcast::<BitcoinRpcError>();
                    assert!(
                        bitcoin_err.is_ok(),
                        "Case {} didn't return BitcoinRpcError: {}",
                        case_idx,
                        err_debug
                    );

                    if let Ok(BitcoinRpcError::BitcoinNodeUnreachable { attempts }) = bitcoin_err {
                        assert_eq!(
                            attempts, *max_retries,
                            "Case {} wrong attempt count",
                            case_idx
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn test_non_connectivity_error_not_retried() {
        let mock_client = MockBitcoinRpcClient::new();
        mock_client.setup_get_raw_transaction_info(
            || {
                Error::JsonRpc(jsonrpc::error::Error::Rpc(jsonrpc::error::RpcError {
                    code: -32600,
                    message: "Invalid request".to_string(),
                    data: None,
                }))
            },
            bitcoincore_rpc::json::GetRawTransactionResult {
                txid: Txid::all_zeros(),
                hash: Wtxid::all_zeros(),
                confirmations: Some(6),
                blockhash: None,
                in_active_chain: None,
                blocktime: None,
                time: None,
                version: 2,
                size: 0,
                vsize: 0,
                locktime: 0,
                vin: vec![],
                vout: vec![],
                hex: vec![],
            },
            None,
        );

        let service = create_test_service(Arc::new(mock_client), 5);

        let result = service
            .is_tx_confirmed("0000000000000000000000000000000000000000000000000000000000000000")
            .await;
        assert!(result.is_err());
    }
}
