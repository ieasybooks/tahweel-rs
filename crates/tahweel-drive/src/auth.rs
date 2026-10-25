use std::{
    fmt, fs,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use tokio::{sync::Mutex, time::Instant};

use crate::{DriveConfig, DriveError, request};

/// A parsed service-account JSON key. Clones share the parsed signing key.
#[derive(Clone)]
pub struct ServiceAccountKey {
    client_email: String,
    key: Arc<EncodingKey>,
}

impl fmt::Debug for ServiceAccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceAccountKey")
            .field("client_email", &self.client_email)
            .field("key", &"[redacted]")
            .finish()
    }
}

impl ServiceAccountKey {
    /// Reads and validates a service-account JSON file. Does not make network requests.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, DriveError> {
        #[derive(Deserialize)]
        struct KeyFile {
            r#type: String,
            client_email: String,
            private_key: String,
        }
        let bytes = fs::read(path.as_ref()).map_err(|err| {
            DriveError::io(
                format!("could not read key file {}", path.as_ref().display()),
                err,
            )
        })?;
        let file: KeyFile = serde_json::from_slice(&bytes).map_err(|_| {
            DriveError::InvalidKey("expected type, client_email and private_key fields")
        })?;
        if file.r#type != "service_account" || file.client_email.trim().is_empty() {
            return Err(DriveError::InvalidKey(
                "expected a service account with a client email",
            ));
        }
        let key =
            EncodingKey::from_rsa_pem(file.private_key.as_bytes()).map_err(DriveError::Signing)?;
        Ok(Self {
            client_email: file.client_email,
            key: Arc::new(key),
        })
    }

    /// Account name used in quota diagnostics.
    pub fn client_email(&self) -> &str {
        &self.client_email
    }

    fn assertion(&self, audience: &str) -> Result<String, DriveError> {
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            scope: &'a str,
            aud: &'a str,
            iat: u64,
            exp: u64,
        }
        let now = unix_time()?;
        jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &Claims {
                iss: &self.client_email,
                scope: "https://www.googleapis.com/auth/drive",
                aud: audience,
                iat: now,
                exp: now + 3600,
            },
            &self.key,
        )
        .map_err(DriveError::Signing)
    }
}

pub(crate) fn unix_time() -> Result<u64, DriveError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| DriveError::Config("system clock is before the Unix epoch".into()))
}

pub(crate) struct Credential {
    key: ServiceAccountKey,
    state: Mutex<TokenState>,
}

struct TokenState {
    token: Option<AccessToken>,
    generation: u64,
    rejected: bool,
}

struct AccessToken {
    value: String,
    refresh_at: Instant,
}

pub(crate) struct Token {
    pub(crate) value: String,
    pub(crate) generation: u64,
}

#[derive(Deserialize)]
pub(crate) struct TokenResponse {
    pub(crate) access_token: String,
    pub(crate) expires_in: u64,
    pub(crate) token_type: String,
}

impl TokenResponse {
    fn into_access_token(self) -> Result<AccessToken, DriveError> {
        if self.access_token.is_empty()
            || !self.token_type.eq_ignore_ascii_case("bearer")
            || self.expires_in == 0
        {
            return Err(DriveError::InvalidResponse(
                "expected a nonempty bearer token and positive expiry",
            ));
        }
        // Short-lived tokens use a proportional margin instead of refreshing immediately.
        let margin = 60.min(self.expires_in / 10);
        let refresh_at = Instant::now()
            .checked_add(Duration::from_secs(self.expires_in - margin))
            .ok_or(DriveError::InvalidResponse("token expiry is out of range"))?;
        Ok(AccessToken {
            value: self.access_token,
            refresh_at,
        })
    }
}

impl Credential {
    pub(crate) fn new(key: ServiceAccountKey) -> Self {
        Self {
            key,
            state: Mutex::new(TokenState {
                token: None,
                generation: 0,
                rejected: false,
            }),
        }
    }

    pub(crate) fn name(&self) -> &str {
        self.key.client_email()
    }

    /// Hold the mutex through refresh so waiting pages reuse the same result. A generation
    /// makes concurrent 401s refresh only once, even if Google returns the same token text.
    pub(crate) async fn token(
        &self,
        client: &reqwest::Client,
        config: &DriveConfig,
        rejected_generation: Option<u64>,
        remember_rejection: bool,
    ) -> Result<Token, DriveError> {
        let mut state = self.state.lock().await;
        if state.rejected {
            return Err(DriveError::CredentialsRejected);
        }
        if let Some(token) = &state.token
            && token.refresh_at > Instant::now()
            && rejected_generation != Some(state.generation)
        {
            return Ok(Token {
                value: token.value.clone(),
                generation: state.generation,
            });
        }
        let result = self.refresh(client, config).await;
        if let Err(error) = &result {
            state.rejected = remember_rejection && error.is_run_level();
        }
        let token = result?;
        let value = token.value.clone();
        state.token = Some(token);
        state.generation += 1;
        Ok(Token {
            value,
            generation: state.generation,
        })
    }

    async fn refresh(
        &self,
        client: &reqwest::Client,
        config: &DriveConfig,
    ) -> Result<AccessToken, DriveError> {
        let assertion = self.key.assertion(&config.token_url)?;
        let request = client.post(&config.token_url).form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", assertion.as_str()),
        ]);
        let bytes = request::token_request(request, config).await?;
        let token: TokenResponse = serde_json::from_slice(&bytes)
            .map_err(|_| DriveError::InvalidResponse("invalid token response"))?;
        token.into_access_token()
    }
}
