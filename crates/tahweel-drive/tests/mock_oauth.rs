//! OAuth flow tests use loopback callbacks and a local token endpoint; no browser is opened.

#[path = "support/transport.rs"]
mod transport;

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Url;
use sha2::{Digest, Sha256};
use tahweel::{ImageFormat, PageImage};
use tahweel_drive::{
    DriveAuth, DriveConfig, DriveError, DriveRecognizer, OAuthToken,
    oauth::{self, OAuthConfig, SignIn},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    task::JoinSet,
};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{header, method, path},
};

fn config(server: &MockServer, dir: &tempfile::TempDir) -> OAuthConfig {
    let mut cfg = OAuthConfig::new(dir.path().join("nested/token.json"));
    cfg.token_url = format!("{}/token", server.uri());
    cfg.callback_timeout = Duration::from_secs(3);
    cfg
}

fn token_response(access: &str, refresh: Option<&str>) -> ResponseTemplate {
    let mut value =
        serde_json::json!({"access_token":access, "expires_in":3600, "token_type":"Bearer"});
    if let Some(refresh) = refresh {
        value["refresh_token"] = refresh.into();
    }
    ResponseTemplate::new(200).set_body_json(value)
}

fn query(url: &Url) -> HashMap<String, String> {
    url.query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn callback(pending: &SignIn, code: &str) -> Url {
    let params = query(pending.authorization_url());
    let mut url = Url::parse(&params["redirect_uri"]).unwrap();
    url.query_pairs_mut().extend_pairs([
        ("iss", "https://accounts.google.com"),
        ("code", code),
        ("state", &params["state"]),
    ]);
    url
}

#[tokio::test]
async fn sign_in_exchanges_pkce_code_and_saves_token_with_unix_permissions() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token_response("access", Some("refresh")))
        .expect(1)
        .mount(&server)
        .await;
    let cfg = config(&server, &dir);
    let pending = SignIn::start(cfg.clone()).await.unwrap();
    let params = query(pending.authorization_url());
    assert_eq!(
        params["scope"],
        "https://www.googleapis.com/auth/drive.file"
    );
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["access_type"], "offline");
    assert_eq!(params["prompt"], "consent");
    assert!(params["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert_eq!(params["state"].len(), 43);
    assert!(!format!("{pending:?}").contains(&params["state"]));
    let url = callback(&pending, "4/a+ b");
    let task = tokio::spawn(pending.finish());
    let browser = reqwest::get(url).await.unwrap();
    assert_eq!(browser.status(), 200);
    let page = browser.text().await.unwrap();
    assert!(page.contains("Signed in"));
    assert!(page.contains("تم تسجيل الدخول"));
    let token = task.await.unwrap().unwrap();
    let debug = format!("{token:?}");
    assert!(!debug.contains("\"access\""));
    assert!(!debug.contains("\"refresh\""));
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&cfg.token_path).unwrap()).unwrap();
    assert_eq!(saved["access_token"], "access");
    assert_eq!(saved["refresh_token"], "refresh");
    assert!(saved["expires_at"].as_u64().unwrap() > 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&cfg.token_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    OAuthToken::load(&cfg.token_path).unwrap();
    let requests = server.received_requests().await.unwrap();
    let form = query(
        &Url::parse(&format!(
            "http://local/?{}",
            String::from_utf8_lossy(&requests[0].body)
        ))
        .unwrap(),
    );
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "4/a+ b");
    assert_eq!(form["redirect_uri"], params["redirect_uri"]);
    assert_eq!(form["client_id"], params["client_id"]);
    assert_ne!(form["client_secret"], "");
    assert_eq!(
        params["code_challenge"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()))
    );
    assert_eq!(form["code_verifier"].len(), 43);
    oauth::sign_out(&cfg.token_path).unwrap();
    oauth::sign_out(&cfg.token_path).unwrap();
    assert!(!cfg.token_path.exists());
}

#[tokio::test]
async fn ignores_favicon_wrong_state_duplicates_and_oversized_headers() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token_response("access", Some("refresh")))
        .expect(1)
        .mount(&server)
        .await;
    let pending = SignIn::start(config(&server, &dir)).await.unwrap();
    let url = callback(&pending, "code");
    let params = query(pending.authorization_url());
    let redirect = Url::parse(&params["redirect_uri"]).unwrap();
    let task = tokio::spawn(pending.finish());
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(redirect.join("favicon.ico").unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .get(redirect.join("?state=wrong&code=no").unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let duplicate = format!(
        "{}?code=no&state={}&state={}",
        redirect, params["state"], params["state"]
    );
    assert_eq!(client.get(duplicate).send().await.unwrap().status(), 400);
    let mut socket = TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
        .await
        .unwrap();
    socket
        .write_all(format!("GET / HTTP/1.1\r\nX-Padding: {}", "x".repeat(9000)).as_bytes())
        .await
        .unwrap();
    let mut reply = vec![0; 256];
    let received = socket.read(&mut reply).await.unwrap();
    assert!(String::from_utf8_lossy(&reply[..received]).starts_with("HTTP/1.1 404"));
    drop(socket);
    assert!(!task.is_finished());
    assert_eq!(client.get(url).send().await.unwrap().status(), 200);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn access_denied_returns_a_bilingual_page_without_exchanging_or_saving() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&server, &dir);
    let pending = SignIn::start(cfg.clone()).await.unwrap();
    let mut url = callback(&pending, "ignored");
    url.query_pairs_mut().append_pair("error", "access_denied");
    let task = tokio::spawn(pending.finish());
    let response = reqwest::get(url).await.unwrap();
    assert_eq!(response.status(), 403);
    let body = response.text().await.unwrap();
    assert!(body.contains("cancelled"));
    assert!(body.contains("إلغاء"));
    assert!(matches!(task.await.unwrap(), Err(DriveError::AccessDenied)));
    assert!(!cfg.token_path.exists());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn incomplete_request_cannot_outlive_callback_timeout() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(&server, &dir);
    cfg.callback_timeout = Duration::from_millis(100);
    let pending = SignIn::start(cfg).await.unwrap();
    let redirect = Url::parse(&query(pending.authorization_url())["redirect_uri"]).unwrap();
    let mut socket = TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
        .await
        .unwrap();
    socket.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
    assert!(matches!(
        pending.finish().await,
        Err(DriveError::SignInTimeout)
    ));
    assert!(
        TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn missing_refresh_token_and_failed_code_exchange_do_not_write_a_file() {
    for status in [200, 400] {
        let server = MockServer::start().await;
        no_leftovers(&server).await;
        let dir = tempfile::tempdir().unwrap();
        let response = if status == 200 {
            token_response("access", None)
        } else {
            ResponseTemplate::new(400)
        };
        Mock::given(method("POST"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let cfg = config(&server, &dir);
        let pending = SignIn::start(cfg.clone()).await.unwrap();
        let url = callback(&pending, "code");
        let task = tokio::spawn(pending.finish());
        assert_eq!(reqwest::get(url).await.unwrap().status(), 400);
        assert!(task.await.unwrap().is_err());
        assert!(!cfg.token_path.exists());
    }
}

fn drive_config(server: &MockServer, token: OAuthToken) -> DriveConfig {
    let mut cfg = DriveConfig::new(DriveAuth::OAuth(token));
    cfg.api_base = server.uri();
    cfg.upload_base = format!("{}/upload", server.uri());
    cfg.token_url = format!("{}/token", server.uri());
    cfg.retry_base_delay = Duration::from_millis(1);
    cfg.retry_max_delay = Duration::from_millis(3);
    cfg
}

fn page() -> PageImage {
    PageImage::new(vec![1, 2, 3], ImageFormat::Jpeg, 1, 1)
}

async fn drive(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(header("authorization", "Bearer new"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"doc"})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/doc/export"))
        .respond_with(ResponseTemplate::new(200).set_body_string("text"))
        .mount(server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/doc"))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}

#[tokio::test]
async fn concurrent_401s_refresh_once_preserve_refresh_token_and_persist() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    let mut token = OAuthToken::new("old", "original-refresh", Duration::from_secs(3600)).unwrap();
    token.save(&token_path).unwrap();
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token_response("new", None).set_delay(Duration::from_millis(30)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(401).set_delay(Duration::from_millis(20)))
        .mount(&server)
        .await;
    drive(&server).await;
    let r = DriveRecognizer::new(drive_config(
        &server,
        OAuthToken::load(&token_path).unwrap(),
    ))
    .await
    .unwrap();
    let mut tasks = JoinSet::new();
    for _ in 0..16 {
        let r = r.clone();
        tasks.spawn(async move { r.recognize_page(&page()).await.unwrap() });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&token_path).unwrap()).unwrap();
    assert_eq!(saved["access_token"], "new");
    assert_eq!(saved["refresh_token"], "original-refresh");
    let requests = server.received_requests().await.unwrap();
    let refresh = requests.iter().find(|r| r.url.path() == "/token").unwrap();
    let form = query(
        &Url::parse(&format!(
            "http://local/?{}",
            String::from_utf8_lossy(&refresh.body)
        ))
        .unwrap(),
    );
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["refresh_token"], "original-refresh");
    assert_ne!(form["client_secret"], "");
}

#[tokio::test]
async fn expired_saved_token_refreshes_at_startup_and_keeps_rotated_refresh_token() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    let mut token = OAuthToken::new("old", "refresh", Duration::ZERO).unwrap();
    token.save(&token_path).unwrap();
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token_response("new", Some("rotated")))
        .expect(1)
        .mount(&server)
        .await;
    let r = DriveRecognizer::new(drive_config(
        &server,
        OAuthToken::load(&token_path).unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(r.config().concurrency_per_account, 4);
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(token_path).unwrap()).unwrap();
    assert_eq!(saved["refresh_token"], "rotated");
}

#[tokio::test]
async fn revoked_sign_in_is_not_retried() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(serde_json::json!({"error":"invalid_grant"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let token = OAuthToken::new("old", "refresh", Duration::ZERO).unwrap();
    let error = DriveRecognizer::new(drive_config(&server, token))
        .await
        .unwrap_err();
    assert!(error.is_run_level());
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn transient_refresh_failure_is_retried_without_discarding_sign_in() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(move |_: &Request| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503)
            } else {
                token_response("new", None)
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let token = OAuthToken::new("old", "refresh", Duration::ZERO).unwrap();
    DriveRecognizer::new(drive_config(&server, token))
        .await
        .unwrap();
}

#[test]
fn saving_replaces_existing_file_atomically_and_sign_out_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("token.json");
    let mut token = OAuthToken::new("access", "refresh", Duration::from_secs(3600)).unwrap();
    token.save(&path).unwrap();
    token.save(&path).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    std::fs::write(&path, b"not JSON").unwrap();
    assert!(matches!(
        OAuthToken::load(&path),
        Err(DriveError::InvalidTokenFile)
    ));
    oauth::sign_out(&path).unwrap();
    assert!(!path.exists());
}

async fn no_leftovers(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"files": []})))
        .with_priority(255)
        .mount(server)
        .await;
}

#[tokio::test]
async fn oauth_startup_cleans_leftovers_with_the_signed_in_account() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .and(header("authorization", "Bearer cached"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"files": [{"id": "old"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/old"))
        .and(header("authorization", "Bearer cached"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let token = OAuthToken::new("cached", "refresh", Duration::from_secs(3600)).unwrap();
    DriveRecognizer::new(drive_config(&server, token))
        .await
        .unwrap();
}

#[tokio::test]
async fn oauth_code_exchange_and_refresh_retry_interrupted_responses() {
    for failure in [
        Vec::new(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{".to_vec(),
    ] {
        let server = MockServer::start().await;
        no_leftovers(&server).await;
        let dir = tempfile::tempdir().unwrap();
        let complete = transport::response(
            200,
            r#"{"access_token":"new","refresh_token":"refresh","expires_in":3600,"token_type":"Bearer"}"#,
        );
        let (base, exchange) = transport::server(vec![failure.clone(), complete.clone()]).await;
        let mut cfg = config(&server, &dir);
        cfg.token_url = format!("{base}/token");
        let pending = SignIn::start(cfg.clone()).await.unwrap();
        let url = callback(&pending, "code");
        let task = tokio::spawn(pending.finish());
        assert_eq!(reqwest::get(url).await.unwrap().status(), 200);
        task.await.unwrap().unwrap();
        assert_eq!(exchange.await.unwrap().len(), 2);
        OAuthToken::load(&cfg.token_path).unwrap();

        let (base, refresh) = transport::server(vec![failure, complete]).await;
        let token = OAuthToken::new("expired", "refresh", Duration::ZERO).unwrap();
        let mut cfg = drive_config(&server, token);
        cfg.token_url = format!("{base}/token");
        DriveRecognizer::new(cfg).await.unwrap();
        assert_eq!(refresh.await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn consumed_authorization_code_stops_when_its_retry_is_rejected() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (base, exchange) = transport::server(vec![
        Vec::new(),
        transport::response(400, r#"{"error":"invalid_grant"}"#),
    ])
    .await;
    let mut cfg = config(&server, &dir);
    cfg.token_url = format!("{base}/token");
    let pending = SignIn::start(cfg.clone()).await.unwrap();
    let url = callback(&pending, "code");
    let task = tokio::spawn(pending.finish());
    assert_eq!(reqwest::get(url).await.unwrap().status(), 400);
    assert!(matches!(
        task.await.unwrap(),
        Err(DriveError::Authentication(_))
    ));
    assert!(!cfg.token_path.exists());
    assert_eq!(exchange.await.unwrap().len(), 2);
}
