use std::{
    fmt, fs,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use tokio::{sync::Mutex, time::Instant};

use crate::{DriveConfig, DriveError, OAuthToken, oauth, request};

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
    kind: CredentialKind,
    state: Arc<Mutex<TokenState>>,
}

enum CredentialKind {
    ServiceAccount(ServiceAccountKey),
    OAuth,
}

struct TokenState {
    token: Option<AccessToken>,
    oauth: Option<OAuthToken>,
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
    pub(crate) refresh_token: Option<String>,
}

impl TokenResponse {
    pub(crate) fn validate(&self) -> Result<(), DriveError> {
        if self.access_token.trim().is_empty()
            || !self.token_type.eq_ignore_ascii_case("bearer")
            || self.expires_in == 0
        {
            return Err(DriveError::InvalidResponse(
                "expected a nonempty bearer token and positive expiry",
            ));
        }
        Ok(())
    }

    fn into_access_token(self) -> Result<AccessToken, DriveError> {
        self.validate()?;
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
            kind: CredentialKind::ServiceAccount(key),
            state: Arc::new(Mutex::new(TokenState {
                token: None,
                oauth: None,
                generation: 0,
                rejected: false,
            })),
        }
    }

    pub(crate) fn oauth(token: OAuthToken) -> Result<Self, DriveError> {
        token.validate()?;
        let remaining = token
            .expires_at
            .saturating_sub(unix_time()?)
            .saturating_sub(60);
        let refresh_at = Instant::now()
            .checked_add(Duration::from_secs(remaining))
            .ok_or(DriveError::InvalidTokenFile)?;
        let access = AccessToken {
            value: token.access_token.clone(),
            refresh_at,
        };
        Ok(Self {
            kind: CredentialKind::OAuth,
            state: Arc::new(Mutex::new(TokenState {
                token: Some(access),
                oauth: Some(token),
                generation: 0,
                rejected: false,
            })),
        })
    }

    pub(crate) fn name(&self) -> &str {
        match &self.kind {
            CredentialKind::ServiceAccount(key) => key.client_email(),
            CredentialKind::OAuth => "Google sign-in",
        }
    }

    pub(crate) fn is_service_account(&self) -> bool {
        matches!(self.kind, CredentialKind::ServiceAccount(_))
    }

    /// Hold the mutex through refresh so concurrent pages reuse its result. The generation
    /// also coalesces 401s when Google happens to return the same access-token text.
    pub(crate) async fn token(
        &self,
        client: &reqwest::Client,
        config: &DriveConfig,
        rejected_generation: Option<u64>,
        remember_rejection: bool,
    ) -> Result<Token, DriveError> {
        let mut state = self.state.clone().lock_owned().await;
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
        let result = self.refresh(client, config, &mut state).await;
        if let Err(error) = &result {
            state.rejected = remember_rejection && error.is_run_level();
        }
        state.token = Some(result?);
        state.generation += 1;
        if state.oauth.as_ref().is_some_and(OAuthToken::has_path) {
            // The blocking write owns the mutex guard. Dropping this caller cannot start a
            // second refresh whose token file might otherwise be overwritten by this write.
            state = tokio::task::spawn_blocking(move || {
                state
                    .oauth
                    .as_mut()
                    .expect("OAuth credential owns an OAuth token")
                    .save_current()?;
                Ok::<_, DriveError>(state)
            })
            .await
            .map_err(|_| DriveError::TokenFileTask)??;
        }
        Ok(Token {
            value: state
                .token
                .as_ref()
                .expect("token was refreshed")
                .value
                .clone(),
            generation: state.generation,
        })
    }

    async fn refresh(
        &self,
        client: &reqwest::Client,
        config: &DriveConfig,
        state: &mut TokenState,
    ) -> Result<AccessToken, DriveError> {
        let request = match &self.kind {
            CredentialKind::ServiceAccount(key) => {
                let assertion = key.assertion(&config.token_url)?;
                client.post(&config.token_url).form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                    ("assertion", assertion.as_str()),
                ])
            }
            CredentialKind::OAuth => {
                let token = state
                    .oauth
                    .as_ref()
                    .expect("OAuth credential owns an OAuth token");
                client.post(&config.token_url).form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", oauth::CLIENT_ID),
                    ("client_secret", oauth::CLIENT_SECRET),
                    ("refresh_token", token.refresh_token.as_str()),
                ])
            }
        };
        let bytes = request::token_request(request, request::Retry::new(config)).await?;
        let response: TokenResponse = serde_json::from_slice(&bytes)
            .map_err(|_| DriveError::InvalidResponse("invalid token response"))?;
        response.validate()?;
        if let Some(token) = &mut state.oauth {
            *token = token.refreshed(&response)?;
        }
        response.into_access_token()
    }
}
