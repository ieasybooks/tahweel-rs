//! Installed-app Google sign-in using a browser and a loopback callback.
//!
//! Use [`SignIn::start`], present [`SignIn::authorization_url`] in your application,
//! then await [`SignIn::finish`]. The caller prints the URL or opens a browser.
//! Signing out deletes the local token file; it does not revoke the Google grant
//! or affect already-running clients.

use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use crate::{
    DriveError,
    auth::{TokenResponse, unix_time},
    config::validate_url,
    request,
};

// This is the public installed-app client already shipped by tahweel.rb.
pub(crate) const CLIENT_ID: &str =
    "512416833080-808aqp20iith31t9rgtdmsgc53jp0sc2.apps.googleusercontent.com";
pub(crate) const CLIENT_SECRET: &str = "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";
const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const MAX_HEAD_BYTES: usize = 8192;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(3);

/// A Google sign-in token and, when loaded or saved, its refresh destination.
#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub(crate) access_token: String,
    pub(crate) refresh_token: String,
    pub(crate) expires_at: u64,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl fmt::Debug for OAuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthToken")
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .field("path", &self.path)
            .finish()
    }
}

impl OAuthToken {
    /// Creates an in-memory token for an app that obtains credentials itself.
    /// Call [`Self::save`] to persist it and remember where refreshes should be written.
    pub fn new(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        expires_in: Duration,
    ) -> Result<Self, DriveError> {
        let token = Self {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            expires_at: unix_time()?
                .checked_add(expires_in.as_secs())
                .ok_or(DriveError::InvalidResponse("token expiry is out of range"))?,
            path: None,
        };
        token.validate()?;
        Ok(token)
    }

    pub(crate) fn validate(&self) -> Result<(), DriveError> {
        if self.refresh_token.trim().is_empty() || self.access_token.trim().is_empty() {
            return Err(DriveError::InvalidTokenFile);
        }
        Ok(())
    }

    /// Reads a saved token. An expired access token is accepted and refreshed by the client.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, DriveError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|err| {
            DriveError::io(
                format!("could not read Google token file {}", path.display()),
                err,
            )
        })?;
        let mut token: Self =
            serde_json::from_slice(&bytes).map_err(|_| DriveError::InvalidTokenFile)?;
        token.validate()?;
        token.path = Some(path.to_owned());
        Ok(token)
    }

    /// Atomically replaces a token file (0600 on Unix) and remembers the path for refreshes.
    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<(), DriveError> {
        let path = path.as_ref();
        self.validate()?;
        let parent = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let write = || -> Result<(), std::io::Error> {
            fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.as_file()
                    .set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            serde_json::to_writer_pretty(&mut file, self)?;
            file.write_all(b"\n")?;
            file.as_file().sync_all()?;
            file.persist(path).map_err(|err| err.error)?;
            Ok(())
        };
        write().map_err(|err| {
            DriveError::io(
                format!("could not write Google token file {}", path.display()),
                err,
            )
        })?;
        self.path = Some(path.to_owned());
        Ok(())
    }

    pub(crate) fn has_path(&self) -> bool {
        self.path.is_some()
    }

    pub(crate) fn save_current(&mut self) -> Result<(), DriveError> {
        if let Some(path) = self.path.clone() {
            self.save(path)?;
        }
        Ok(())
    }

    pub(crate) async fn persist_refresh(mut self) -> Result<Self, DriveError> {
        if let Some(path) = self.path.clone() {
            tokio::task::spawn_blocking(move || {
                self.save(path)?;
                Ok(self)
            })
            .await
            .map_err(|_| DriveError::TokenFileTask)?
        } else {
            Ok(self)
        }
    }

    pub(crate) fn refreshed(&self, response: &TokenResponse) -> Result<Self, DriveError> {
        let mut next = Self::new(
            response.access_token.clone(),
            response
                .refresh_token
                .as_deref()
                .unwrap_or(&self.refresh_token),
            Duration::from_secs(response.expires_in),
        )?;
        next.path.clone_from(&self.path);
        Ok(next)
    }
}

/// Default persistent location: the user's local data directory plus `tahweel/google-token.json`.
pub fn default_token_path() -> Result<PathBuf, DriveError> {
    dirs::data_local_dir()
        .map(|dir| dir.join("tahweel/google-token.json"))
        .ok_or_else(|| {
            DriveError::Config(
                "could not locate the local data directory; choose a token file path".into(),
            )
        })
}

/// Deletes the saved sign-in. A missing file is already signed out.
pub fn sign_out(path: impl AsRef<Path>) -> Result<(), DriveError> {
    match fs::remove_file(path.as_ref()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(DriveError::io(
            format!(
                "could not delete Google token file {}",
                path.as_ref().display()
            ),
            err,
        )),
    }
}

/// Browser sign-in settings with configurable endpoints for local tests.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OAuthConfig {
    /// Destination of the token JSON file.
    pub token_path: PathBuf,
    /// Time allowed for the browser callback (default five minutes).
    pub callback_timeout: Duration,
    /// Timeout for each token request (default two minutes).
    pub request_timeout: Duration,
    /// Google consent endpoint.
    pub authorization_url: String,
    /// Google token endpoint.
    pub token_url: String,
}

impl OAuthConfig {
    /// Settings for a caller-selected token file. See [`default_token_path`].
    pub fn new(token_path: impl Into<PathBuf>) -> Self {
        Self {
            token_path: token_path.into(),
            callback_timeout: Duration::from_secs(300),
            request_timeout: Duration::from_secs(120),
            authorization_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
        }
    }
}

/// A pending sign-in. Dropping it closes the loopback listener.
pub struct SignIn {
    config: OAuthConfig,
    client: reqwest::Client,
    listener: TcpListener,
    url: Url,
    redirect: String,
    verifier: String,
    state: String,
}

impl fmt::Debug for SignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignIn")
            .field("config", &self.config)
            .field("redirect", &self.redirect)
            .finish_non_exhaustive()
    }
}

impl SignIn {
    /// Opens a random port on `127.0.0.1` and builds the consent URL, without opening a browser.
    pub async fn start(config: OAuthConfig) -> Result<Self, DriveError> {
        validate_url(&config.authorization_url)?;
        validate_url(&config.token_url)?;
        if config.callback_timeout.is_zero()
            || config.request_timeout.is_zero()
            || config.token_path.as_os_str().is_empty()
        {
            return Err(DriveError::Config(
                "sign-in timeouts and token path must be nonempty".into(),
            ));
        }
        let client = request::client(config.request_timeout)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|err| DriveError::io("could not start Google sign-in listener", err))?;
        let address = listener
            .local_addr()
            .map_err(|err| DriveError::io("could not read Google sign-in listener address", err))?;
        let redirect = format!("http://{address}/");
        let verifier = random_token();
        let state = random_token();
        let mut url =
            Url::parse(&config.authorization_url).expect("authorization URL is validated");
        url.query_pairs_mut().extend_pairs([
            ("client_id", CLIENT_ID),
            ("redirect_uri", redirect.as_str()),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge(&verifier).as_str()),
            ("state", state.as_str()),
        ]);
        Ok(Self {
            config,
            client,
            listener,
            url,
            redirect,
            verifier,
            state,
        })
    }

    /// URL to show in a browser or the calling application's UI.
    pub fn authorization_url(&self) -> &Url {
        &self.url
    }

    /// Waits for the matching callback, exchanges its code and saves the token atomically.
    pub async fn finish(self) -> Result<OAuthToken, DriveError> {
        let (code, mut stream) =
            tokio::time::timeout(self.config.callback_timeout, self.callback())
                .await
                .map_err(|_| DriveError::SignInTimeout)??;
        let result = self.exchange(&code).await;
        let (status, body) = if result.is_ok() {
            ("200 OK", SUCCESS)
        } else {
            ("400 Bad Request", FAILED)
        };
        // A browser closing its tab must not discard a successfully saved token.
        let _ = tokio::time::timeout(CONNECTION_TIMEOUT, answer(&mut stream, status, body)).await;
        result
    }

    async fn callback(&self) -> Result<(String, TcpStream), DriveError> {
        loop {
            let (mut stream, _) =
                self.listener.accept().await.map_err(|err| {
                    DriveError::io("could not accept Google sign-in callback", err)
                })?;
            let Ok(Ok(outcome)) =
                tokio::time::timeout(CONNECTION_TIMEOUT, read_callback(&mut stream, &self.state))
                    .await
            else {
                continue;
            };
            let (status, body) = match outcome {
                Callback::Code(code) => return Ok((code, stream)),
                Callback::Denied => {
                    let _ = tokio::time::timeout(
                        CONNECTION_TIMEOUT,
                        answer(&mut stream, "403 Forbidden", DENIED),
                    )
                    .await;
                    return Err(DriveError::AccessDenied);
                }
                Callback::Failed => {
                    let _ = tokio::time::timeout(
                        CONNECTION_TIMEOUT,
                        answer(&mut stream, "400 Bad Request", FAILED),
                    )
                    .await;
                    return Err(DriveError::SignInFailed);
                }
                Callback::Mismatch => ("400 Bad Request", UNRELATED),
                Callback::Unrelated => ("404 Not Found", UNRELATED),
            };
            let _ =
                tokio::time::timeout(CONNECTION_TIMEOUT, answer(&mut stream, status, body)).await;
        }
    }

    async fn exchange(&self, code: &str) -> Result<OAuthToken, DriveError> {
        let request = self.client.post(&self.config.token_url).form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("code", code),
            ("code_verifier", self.verifier.as_str()),
            ("redirect_uri", self.redirect.as_str()),
        ]);
        // A lost response can consume the code; a rejected retry asks the caller
        // to start sign-in again. Transport failures still get the normal retry budget.
        let bytes = request::token_request(request, request::Retry::default()).await?;
        let response: TokenResponse = serde_json::from_slice(&bytes)
            .map_err(|_| DriveError::InvalidResponse("invalid sign-in token response"))?;
        response.validate()?;
        let refresh = response.refresh_token.ok_or(DriveError::InvalidResponse(
            "Google did not return a refresh token",
        ))?;
        let mut token = OAuthToken::new(
            response.access_token,
            refresh,
            Duration::from_secs(response.expires_in),
        )?;
        token.path = Some(self.config.token_path.clone());
        token.persist_refresh().await
    }
}

fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

enum Callback {
    Code(String),
    Denied,
    Failed,
    Mismatch,
    Unrelated,
}

async fn read_callback(stream: &mut TcpStream, state: &str) -> std::io::Result<Callback> {
    let mut head = Vec::with_capacity(512);
    // Read only the request head, never an HTTP body, and never more than 8 KiB.
    while head.len() < MAX_HEAD_BYTES {
        let byte = stream.read_u8().await?;
        head.push(byte);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(parse_callback(&head, state));
        }
    }
    Ok(Callback::Unrelated)
}

fn parse_callback(head: &[u8], expected_state: &str) -> Callback {
    let Ok(head) = std::str::from_utf8(head) else {
        return Callback::Unrelated;
    };
    let mut line = head.lines().next().unwrap_or_default().split_whitespace();
    if line.next() != Some("GET") {
        return Callback::Unrelated;
    }
    let Some(target) = line.next() else {
        return Callback::Unrelated;
    };
    if !matches!(line.next(), Some("HTTP/1.0" | "HTTP/1.1"))
        || line.next().is_some()
        || !target.starts_with('/')
        || target.starts_with("//")
    {
        return Callback::Unrelated;
    }
    let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Callback::Unrelated;
    };
    if url.path() != "/" {
        return Callback::Unrelated;
    }
    let (mut code, mut error, mut state) = (None, None, None);
    for (key, value) in url.query_pairs() {
        let field = match key.as_ref() {
            "code" => &mut code,
            "error" => &mut error,
            "state" => &mut state,
            _ => continue,
        };
        if field.replace(value.into_owned()).is_some() {
            return Callback::Mismatch;
        }
    }
    if code.is_none() && error.is_none() {
        return Callback::Unrelated;
    }
    if state.as_deref() != Some(expected_state) {
        return Callback::Mismatch;
    }
    if let Some(error) = error {
        return if error == "access_denied" {
            Callback::Denied
        } else {
            Callback::Failed
        };
    }
    match code {
        Some(code) if !code.is_empty() => Callback::Code(code),
        _ => Callback::Unrelated,
    }
}

async fn answer(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

const SUCCESS: &str = "<!doctype html><meta charset=\"utf-8\"><title>Tahweel</title><p lang=\"ar\" dir=\"rtl\">تم تسجيل الدخول. يمكنك إغلاق هذه النافذة والعودة إلى تحويل.</p><p lang=\"en\">Signed in. You can close this window and return to Tahweel.</p>";
const DENIED: &str = "<!doctype html><meta charset=\"utf-8\"><title>Tahweel</title><p lang=\"ar\" dir=\"rtl\">تم إلغاء تسجيل الدخول.</p><p lang=\"en\">Sign-in was cancelled. You can close this window.</p>";
const FAILED: &str = "<!doctype html><meta charset=\"utf-8\"><title>Tahweel</title><p lang=\"ar\" dir=\"rtl\">تعذر تسجيل الدخول. عد إلى تحويل للمحاولة مرة أخرى.</p><p lang=\"en\">Sign-in failed. Return to Tahweel to try again.</p>";
const UNRELATED: &str = "<!doctype html><meta charset=\"utf-8\"><title>Tahweel</title><p lang=\"ar\" dir=\"rtl\">يرجى إكمال تسجيل الدخول في نافذة Google.</p><p lang=\"en\">Please finish signing in in the Google window.</p>";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pkce_matches_rfc_7636() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(random_token().len(), 43);
        assert_ne!(random_token(), random_token());
    }
}
