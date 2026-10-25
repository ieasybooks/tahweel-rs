use std::{sync::OnceLock, time::Duration};

use aws_lc_rs::{
    encoding::AsDer,
    rsa::{KeyPair, KeySize},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use tahweel::{ImageFormat, PageImage};
use tahweel_drive::{DriveAuth, DriveConfig, ServiceAccountKey};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

// Fresh, test-only keys are generated in memory. No real credential is needed or committed.
pub(crate) fn key_pair() -> &'static KeyPair {
    static KEY: OnceLock<KeyPair> = OnceLock::new();
    KEY.get_or_init(|| KeyPair::generate(KeySize::Rsa2048).unwrap())
}

pub(crate) fn key(account: &str) -> ServiceAccountKey {
    ServiceAccountKey::from_json(&key_json(account)).unwrap()
}

pub(crate) fn key_json(account: &str) -> String {
    let der = key_pair().as_der().unwrap();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        STANDARD.encode(der.as_ref())
    );
    let json =
        serde_json::json!({"type": "service_account", "client_email": account, "private_key": pem});
    json.to_string()
}

pub(crate) fn config(server: &MockServer, accounts: &[&str]) -> DriveConfig {
    let mut config = DriveConfig::new(DriveAuth::ServiceAccounts(
        accounts.iter().map(|account| key(account)).collect(),
    ));
    config.api_base = server.uri();
    config.upload_base = format!("{}/upload", server.uri());
    config.token_url = format!("{}/token", server.uri());
    config.retry_base_delay = Duration::from_millis(1);
    config.retry_max_delay = Duration::from_millis(3);
    config
}

pub(crate) fn page() -> PageImage {
    PageImage::new(vec![0xff, 0xd8, 0, 42, 0xff, 0xd9], ImageFormat::Jpeg, 1, 1)
}

pub(crate) fn token(value: &str, expires_in: u64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(
        serde_json::json!({"access_token":value, "expires_in":expires_in, "token_type":"Bearer"}),
    )
}

pub(crate) async fn tokens(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token("access", 3600))
        .mount(server)
        .await;
}

pub(crate) async fn no_leftovers(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"files": []})))
        .with_priority(255)
        .mount(server)
        .await;
}

pub(crate) fn uploaded() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"doc"}))
}

pub(crate) async fn export_and_delete(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/doc/export"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("\u{feff}________________\r\n\r\nنص\r\nعربي\r\n"),
        )
        .mount(server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/doc"))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}

pub(crate) async fn success(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(uploaded())
        .mount(server)
        .await;
    export_and_delete(server).await;
}

pub(crate) fn failure(status: u16, reason: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(
        serde_json::json!({"error":{"errors":[{"reason":"unrelated"},{"reason":reason}]}}),
    )
}
