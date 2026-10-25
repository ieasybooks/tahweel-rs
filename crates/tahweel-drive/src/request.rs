use std::time::{Duration, SystemTime};

use bytes::Bytes;
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

use crate::{DriveConfig, DriveError};

/// Rate limits do not spend ordinary attempts, but a permanently held lane must terminate.
const MAX_RATE_LIMIT_RETRIES: u32 = 60;

pub(crate) const DEFAULT_MAX_ATTEMPTS: u32 = 5;
pub(crate) const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
pub(crate) const DEFAULT_RETRY_MAX_DELAY: Duration = Duration::from_secs(60);

pub(crate) fn client(timeout: Duration) -> Result<Client, DriveError> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10).min(timeout))
        .read_timeout(Duration::from_secs(60).min(timeout))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("tahweel/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(DriveError::Client)
}

pub(crate) async fn send(request: RequestBuilder) -> Result<Bytes, DriveError> {
    #[derive(Deserialize, Default)]
    struct Reason {
        reason: String,
    }
    #[derive(Deserialize, Default)]
    struct Detail {
        #[serde(default)]
        errors: Vec<Reason>,
        status: Option<String>,
    }
    #[derive(Deserialize)]
    struct Envelope {
        error: Detail,
    }
    let response = request
        .send()
        .await
        .map_err(|err| DriveError::Request(err.without_url()))?;
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after);
    let bytes = response
        .bytes()
        .await
        .map_err(|err| DriveError::Request(err.without_url()))?;
    if status.is_success() {
        return Ok(bytes);
    }
    let detail = serde_json::from_slice::<Envelope>(&bytes)
        .map(|v| v.error)
        .unwrap_or_default();
    Err(DriveError::Status {
        status: status.as_u16(),
        reasons: detail.errors.into_iter().map(|v| v.reason).collect(),
        api_status: detail.status,
        retry_after,
    })
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse() {
        return Some(Duration::from_secs(seconds));
    }
    httpdate::parse_http_date(value)
        .ok()
        .map(|at| at.duration_since(SystemTime::now()).unwrap_or_default())
}

pub(crate) struct Retry {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
    failures: u32,
    rate_limits: u32,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay: DEFAULT_RETRY_BASE_DELAY,
            max_delay: DEFAULT_RETRY_MAX_DELAY,
            failures: 0,
            rate_limits: 0,
        }
    }
}

impl Retry {
    pub(crate) fn new(config: &DriveConfig) -> Self {
        Self {
            max_attempts: config.max_attempts,
            base_delay: config.retry_base_delay,
            max_delay: config.retry_max_delay,
            failures: 0,
            rate_limits: 0,
        }
    }

    pub(crate) fn delay(&mut self, error: &DriveError) -> Option<Duration> {
        if !error.is_retryable() {
            return None;
        }
        let exponent = if error.is_rate_limited() {
            if self.rate_limits >= MAX_RATE_LIMIT_RETRIES {
                return None;
            }
            self.rate_limits += 1;
            self.rate_limits - 1
        } else {
            self.failures += 1;
            if self.failures >= self.max_attempts {
                return None;
            }
            self.failures - 1
        };
        let ceiling = self
            .base_delay
            .saturating_mul(1 << exponent.min(31))
            .min(self.max_delay);
        Some(
            error
                .retry_after()
                .unwrap_or_else(|| ceiling.mul_f64(rand::random::<f64>())),
        )
    }
}

pub(crate) async fn token_request(
    request: RequestBuilder,
    mut retry: Retry,
) -> Result<Bytes, DriveError> {
    loop {
        match send(request.try_clone().expect("token form body is cloneable")).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => match retry.delay(&error) {
                Some(delay) => tokio::time::sleep(delay).await,
                None if !error.is_retryable() => {
                    return Err(DriveError::Authentication(Box::new(error)));
                }
                None => return Err(error),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_supports_seconds_and_dates() {
        assert_eq!(parse_retry_after(" 12 "), Some(Duration::from_secs(12)));
        let future = SystemTime::now() + Duration::from_secs(30);
        let delay = parse_retry_after(&httpdate::fmt_http_date(future)).unwrap();
        assert!((29..=30).contains(&delay.as_secs()));
        assert_eq!(parse_retry_after("invalid"), None);
    }
}
