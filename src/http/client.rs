use std::time::Duration;

use reqwest::StatusCode;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{
    default_on_request_failure, policies::ExponentialBackoff, RetryTransientMiddleware, Retryable,
    RetryableStrategy,
};

use crate::errors::{AppError, AppResult};

#[derive(Debug, Clone, Copy)]
struct NetworkAnd5xxOnly;

impl RetryableStrategy for NetworkAnd5xxOnly {
    fn handle(
        &self,
        res: &Result<reqwest::Response, reqwest_middleware::Error>,
    ) -> Option<Retryable> {
        match res {
            Ok(success) if success.status().is_server_error() => Some(Retryable::Transient),
            Ok(success) if success.status() == StatusCode::REQUEST_TIMEOUT => {
                Some(Retryable::Transient)
            }
            Ok(_) => None,
            Err(err) => default_on_request_failure(err),
        }
    }
}

pub fn build_raw_client(timeout: Duration) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| AppError::Http(format!("http client init failed: {e}")))
}

pub fn build_retrying_client(
    timeout: Duration,
    max_retries: u32,
) -> AppResult<ClientWithMiddleware> {
    let raw = build_raw_client(timeout)?;
    let retry_policy = ExponentialBackoff::builder().build_with_max_retries(max_retries);

    Ok(ClientBuilder::new(raw)
        .with(RetryTransientMiddleware::new_with_policy_and_strategy(
            retry_policy,
            NetworkAnd5xxOnly,
        ))
        .build())
}
