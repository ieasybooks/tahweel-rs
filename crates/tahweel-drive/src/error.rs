use std::time::Duration;

/// Failures from Drive, its credentials or local sign-in files.
///
/// Response bodies are deliberately omitted: Google errors can include request data.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DriveError {
    /// Invalid caller settings.
    #[error("{0}")]
    Config(String),
    /// A local file or socket operation failed.
    #[error("{context}")]
    Io {
        /// Operation being performed.
        context: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Could not construct the shared HTTP client.
    #[error("could not create HTTP client")]
    Client(#[source] reqwest::Error),
    /// Could not send a request or read its response.
    #[error("Google request failed")]
    Request(#[source] reqwest::Error),
    /// Google returned an unsuccessful HTTP status.
    #[error("Google returned HTTP {status}")]
    Status {
        /// HTTP status.
        status: u16,
        /// Google's reason codes, without its response message or body.
        reasons: Vec<String>,
        /// Google's structured status code, if present.
        api_status: Option<String>,
        /// Minimum delay requested by Google.
        retry_after: Option<Duration>,
    },
    /// Malformed JSON or missing fields in a Google response.
    #[error("unexpected Google response: {0}")]
    InvalidResponse(&'static str),
    /// A key file has an invalid shape or account type.
    #[error("invalid service-account key: {0}")]
    InvalidKey(&'static str),
    /// Could not parse a signing key or sign the service-account assertion.
    #[error("could not use the service-account signing key")]
    Signing(#[source] jsonwebtoken::errors::Error),
    /// Google rejected the credential. The caller should stop the run.
    #[error("Google rejected the credentials; sign in again or check the service-account key")]
    Authentication(#[source] Box<DriveError>),
    /// Another page already discovered rejected credentials.
    #[error("Google credentials were rejected; stop the run and sign in again or check the key")]
    CredentialsRejected,
    /// Every service-account lane has run out of Drive storage.
    #[error("no Google Drive account has storage available; use another account or Google sign-in")]
    NoAccounts,
    /// The saved sign-in cannot be read as a complete Google token.
    #[error("invalid Google token file; sign in again")]
    InvalidTokenFile,
    /// A background token-file write could not finish.
    #[error("could not finish writing the Google token file")]
    TokenFileTask,
    /// The browser did not return before the callback deadline.
    #[error("Google sign-in timed out")]
    SignInTimeout,
    /// The user declined the Google consent screen.
    #[error("Google sign-in was cancelled")]
    AccessDenied,
    /// Google returned an unsuccessful sign-in callback.
    #[error("Google sign-in failed; try signing in again")]
    SignInFailed,
    /// The encoded page exceeds the configured upload limit.
    #[error("page image has {bytes} bytes, exceeding the {limit}-byte limit")]
    ImageTooLarge {
        /// Actual byte count.
        bytes: usize,
        /// Configured limit.
        limit: usize,
    },
}

impl DriveError {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// Whether a further attempt of the same HTTP operation may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Request(_) => true,
            Self::Status { status, .. } => {
                matches!(status, 408 | 429 | 500..=599) || self.is_rate_limited()
            }
            _ => false,
        }
    }

    /// Server-requested retry delay, including HTTP-date `Retry-After` values.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Whether continuing with other documents would repeat a run-wide failure.
    pub fn is_run_level(&self) -> bool {
        matches!(
            self,
            Self::Authentication(_)
                | Self::CredentialsRejected
                | Self::NoAccounts
                | Self::InvalidKey(_)
                | Self::Signing(_)
                | Self::Config(_)
                | Self::InvalidTokenFile
        )
    }

    pub(crate) fn is_rate_limited(&self) -> bool {
        match self {
            Self::Status { status: 429, .. } => true,
            Self::Status {
                status: 403,
                reasons,
                api_status,
                ..
            } => {
                !reasons
                    .iter()
                    .any(|s| matches!(s.as_str(), "storageQuotaExceeded" | "dailyLimitExceeded"))
                    && (reasons.iter().any(|s| {
                        matches!(s.as_str(), "rateLimitExceeded" | "userRateLimitExceeded")
                    }) || api_status.as_deref() == Some("RESOURCE_EXHAUSTED"))
            }
            _ => false,
        }
    }

    pub(crate) fn is_storage_quota(&self) -> bool {
        matches!(self, Self::Status { status: 403, reasons, .. } if reasons.iter().any(|s| s == "storageQuotaExceeded"))
    }

    pub(crate) fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Status { status: 401, .. })
    }
}
