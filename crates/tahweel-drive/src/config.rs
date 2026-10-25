use std::time::Duration;

use crate::request::{DEFAULT_MAX_ATTEMPTS, DEFAULT_RETRY_BASE_DELAY, DEFAULT_RETRY_MAX_DELAY};
use crate::{DriveError, OAuthToken, ServiceAccountKey};

/// Credentials used by the recognizer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DriveAuth {
    /// One lane per service account. Load keys with [`ServiceAccountKey::from_file`].
    ServiceAccounts(Vec<ServiceAccountKey>),
    /// One lane for the user signed in through the browser.
    OAuth(OAuthToken),
}

/// Google Drive settings. Start with [`Self::new`] and adjust the public fields.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DriveConfig {
    /// Credentials; their `Debug` implementations omit key and token contents.
    pub auth: DriveAuth,
    /// OCR language, e.g. `ar`.
    pub language: String,
    /// Maximum concurrent page operations per account, including cleanup.
    pub concurrency_per_account: usize,
    /// Attempts per HTTP operation, including the first; rate limits have a separate budget.
    pub max_attempts: u32,
    /// Maximum encoded image size, in bytes.
    pub max_image_bytes: usize,
    /// Timeout for each complete HTTP request, including its response body.
    pub request_timeout: Duration,
    /// Initial ceiling for full-jitter retry delays.
    pub retry_base_delay: Duration,
    /// Maximum ceiling for full-jitter retry delays; `Retry-After` can exceed it.
    pub retry_max_delay: Duration,
    /// API origin, normally `https://www.googleapis.com`.
    pub api_base: String,
    /// Upload base, normally `https://www.googleapis.com/upload`.
    pub upload_base: String,
    /// Token exchange endpoint.
    pub token_url: String,
}

impl DriveConfig {
    /// Defaults for these credentials: Arabic, four pages per account and five attempts.
    pub fn new(auth: DriveAuth) -> Self {
        Self {
            auth,
            language: "ar".into(),
            concurrency_per_account: 4,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            max_image_bytes: 2_000_000,
            request_timeout: Duration::from_secs(120),
            retry_base_delay: DEFAULT_RETRY_BASE_DELAY,
            retry_max_delay: DEFAULT_RETRY_MAX_DELAY,
            api_base: "https://www.googleapis.com".into(),
            upload_base: "https://www.googleapis.com/upload".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), DriveError> {
        let accounts = match &self.auth {
            DriveAuth::ServiceAccounts(keys) => keys.len(),
            DriveAuth::OAuth(token) => {
                token.validate()?;
                1
            }
        };
        if accounts == 0 {
            return Err(DriveError::Config(
                "at least one service account is required".into(),
            ));
        }
        if self.concurrency_per_account == 0 || self.max_attempts == 0 {
            return Err(DriveError::Config(
                "concurrency and attempts must be at least 1".into(),
            ));
        }
        if accounts.checked_mul(self.concurrency_per_account).is_none() {
            return Err(DriveError::Config("total concurrency is too large".into()));
        }
        if self.language.trim().is_empty()
            || self.max_image_bytes == 0
            || self.request_timeout.is_zero()
        {
            return Err(DriveError::Config(
                "language, image limit and timeout must be nonempty".into(),
            ));
        }
        if self.retry_base_delay > self.retry_max_delay {
            return Err(DriveError::Config(
                "retry base delay exceeds its maximum".into(),
            ));
        }
        for url in [&self.api_base, &self.upload_base, &self.token_url] {
            validate_url(url)?;
        }
        Ok(())
    }
}

pub(crate) fn validate_url(value: &str) -> Result<(), DriveError> {
    let valid = reqwest::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    });
    if valid {
        Ok(())
    } else {
        Err(DriveError::Config(
            "endpoint must be an HTTP(S) URL without credentials, query or fragment".into(),
        ))
    }
}
