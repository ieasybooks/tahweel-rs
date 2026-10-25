//! Google Drive integration tests. All requests stay on local mock servers.

mod common;
#[path = "support/transport.rs"]
mod transport;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use aws_lc_rs::signature::KeyPair;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use common::*;
use tahweel::{ImageFormat, PageImage, Recognizer};
use tahweel_drive::{DriveAuth, DriveConfig, DriveError, DriveRecognizer, ServiceAccountKey};
use tokio::{sync::Notify, task::JoinSet};
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

#[tokio::test]
async fn uploads_related_multipart_exports_cleans_and_deletes() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(query_param("uploadType", "multipart"))
        .and(query_param("ocrLanguage", "ar"))
        .and(query_param("fields", "id"))
        .and(header("authorization", "Bearer access"))
        .respond_with(uploaded())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/doc/export"))
        .and(query_param("mimeType", "text/plain"))
        .and(header("authorization", "Bearer access"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("\u{feff}________________\r\n\r\n بسم الله\r\nسطر\rآخر\r\n"),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/doc"))
        .and(header("authorization", "Bearer access"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let recognizer = DriveRecognizer::new(config(&server, &["one@example.test"]))
        .await
        .unwrap();
    assert_eq!(recognizer.max_in_flight(), 8);
    assert_eq!(recognizer.max_image_bytes(), Some(2_000_000));
    let image = page();
    assert_eq!(
        recognizer.recognize(&image).await.unwrap().text,
        "بسم الله\nسطر\nآخر"
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 5);
    let upload = &requests[2];
    let content_type = upload
        .headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    let boundary = content_type
        .strip_prefix("multipart/related; boundary=")
        .unwrap();
    let prefix = format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n");
    assert!(upload.body.starts_with(prefix.as_bytes()));
    let separator = format!("\r\n--{boundary}\r\nContent-Type: image/jpeg\r\n\r\n");
    let end = upload
        .body
        .windows(separator.len())
        .position(|bytes| bytes == separator.as_bytes())
        .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_slice(&upload.body[prefix.len()..end]).unwrap();
    assert_eq!(metadata["mimeType"], "application/vnd.google-apps.document");
    assert_eq!(
        metadata["appProperties"],
        serde_json::json!({"tahweel": "ocr"})
    );
    assert!(metadata["name"].as_str().unwrap().starts_with("tahweel-"));
    let body = &upload.body[end + separator.len()..];
    assert!(body.starts_with(&image.data));
    assert_eq!(
        &body[image.data.len()..],
        format!("\r\n--{boundary}--\r\n").as_bytes()
    );
}

#[tokio::test]
async fn uploads_without_a_language_hint_when_drive_detects_the_language() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(query_param("uploadType", "multipart"))
        .and(query_param("fields", "id"))
        .and(|request: &Request| {
            request
                .url
                .query_pairs()
                .all(|(key, _)| key != "ocrLanguage")
        })
        .respond_with(uploaded())
        .expect(1)
        .mount(&server)
        .await;
    export_and_delete(&server).await;
    let mut cfg = config(&server, &["one@example.test"]);
    cfg.language = None;
    let recognizer = DriveRecognizer::new(cfg).await.unwrap();
    assert_eq!(
        recognizer.recognize(&page()).await.unwrap().text,
        "نص\nعربي"
    );
}

#[tokio::test]
async fn service_account_assertion_has_correct_signature_claims_and_scope() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    DriveRecognizer::new(config(&server, &["one@example.test"]))
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    let form = reqwest::Url::parse(&format!(
        "http://local/?{}",
        String::from_utf8_lossy(&requests[0].body)
    ))
    .unwrap();
    let form: std::collections::HashMap<_, _> = form.query_pairs().collect();
    assert_eq!(
        form["grant_type"],
        "urn:ietf:params:oauth:grant-type:jwt-bearer"
    );
    let decoding = jsonwebtoken::DecodingKey::from_rsa_der(key_pair().public_key().as_ref());
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_audience(&[format!("{}/token", server.uri())]);
    let jwt = jsonwebtoken::decode::<serde_json::Value>(
        form["assertion"].as_bytes(),
        &decoding,
        &validation,
    )
    .unwrap();
    assert_eq!(jwt.claims["iss"], "one@example.test");
    assert_eq!(jwt.claims["scope"], "https://www.googleapis.com/auth/drive");
    assert_eq!(
        jwt.claims["exp"].as_u64().unwrap() - jwt.claims["iat"].as_u64().unwrap(),
        3600
    );
}

#[test]
fn service_account_json_and_file_load_the_same_key_and_ignore_extra_fields() {
    let json = key_json("one@example.test");
    let mut document: serde_json::Value = serde_json::from_str(&json).unwrap();
    document["project_id"] = serde_json::json!("unused-project");
    document["extra"] = serde_json::json!({"ignored": true});
    let from_json = ServiceAccountKey::from_json(&document.to_string()).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &json).unwrap();
    let from_file = ServiceAccountKey::from_file(file.path()).unwrap();
    assert_eq!(from_json.client_email(), "one@example.test");
    assert_eq!(from_json.client_email(), from_file.client_email());
    for key in [from_json, from_file] {
        let debug = format!("{key:?}");
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains(document["private_key"].as_str().unwrap()));
        assert!(!debug.contains("PRIVATE KEY"));
    }
}

#[test]
fn service_account_json_rejects_invalid_input_without_echoing_it() {
    let key_text = "key-text-that-must-not-appear-in-errors";
    for json in [
        "{invalid-json".into(),
        "null".into(),
        "{}".into(),
        serde_json::json!({"type": "service_account", "client_email": "one"}).to_string(),
        serde_json::json!({"type": "service_account", "private_key": key_text}).to_string(),
        serde_json::json!({"client_email": "one", "private_key": key_text}).to_string(),
        serde_json::json!({"type": "other", "client_email": "one", "private_key": key_text}).to_string(),
        serde_json::json!({"type": "service_account", "client_email": " ", "private_key": key_text}).to_string(),
        serde_json::json!({"type": "service_account", "client_email": 42, "private_key": key_text}).to_string(),
        serde_json::json!({"type": "service_account", "client_email": "one", "private_key": key_text}).to_string(),
        serde_json::json!({"type": "service_account", "client_email": "one", "private_key": ""}).to_string(),
    ] {
        let error = ServiceAccountKey::from_json(&json).unwrap_err();
        assert!(matches!(error, DriveError::InvalidKey(_) | DriveError::Signing(_)));
        assert!(!format!("{error:?}").contains(key_text));
        assert!(!error.to_string().contains(key_text));
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), &json).unwrap();
        let file_error = ServiceAccountKey::from_file(file.path()).unwrap_err();
        assert_eq!(std::mem::discriminant(&error), std::mem::discriminant(&file_error));
    }
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), [0xff]).unwrap();
    assert!(matches!(
        ServiceAccountKey::from_file(file.path()),
        Err(DriveError::InvalidKey(_))
    ));
}

#[tokio::test]
async fn retry_classes_and_rate_limit_budget() {
    for (status, reason, rate_limit) in [
        (429, "", true),
        (403, "rateLimitExceeded", true),
        (403, "userRateLimitExceeded", true),
        (408, "", false),
        (500, "", false),
        (502, "", false),
        (503, "", false),
        (504, "", false),
    ] {
        let server = MockServer::start().await;
        no_leftovers(&server).await;
        tokens(&server).await;
        Mock::given(method("POST"))
            .and(path("/upload/drive/v3/files"))
            .respond_with(failure(status, reason))
            .up_to_n_times(2)
            .with_priority(1)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/upload/drive/v3/files"))
            .respond_with(uploaded())
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;
        export_and_delete(&server).await;
        let mut cfg = config(&server, &["one"]);
        cfg.max_attempts = if rate_limit { 1 } else { 3 };
        DriveRecognizer::new(cfg)
            .await
            .unwrap()
            .recognize_page(&page())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn attempts_and_rate_limit_ceiling_are_bounded() {
    for (status, attempts, calls) in [
        (503, 3, 3),
        (429, 1, 61),
        (400, 5, 1),
        (404, 5, 1),
        (403, 5, 1),
    ] {
        let server = MockServer::start().await;
        no_leftovers(&server).await;
        tokens(&server).await;
        Mock::given(method("POST"))
            .and(path("/upload/drive/v3/files"))
            .respond_with(failure(status, "dailyLimitExceeded"))
            .expect(calls)
            .mount(&server)
            .await;
        let mut cfg = config(&server, &["one"]);
        cfg.max_attempts = attempts;
        let error = DriveRecognizer::new(cfg)
            .await
            .unwrap()
            .recognize_page(&page())
            .await
            .unwrap_err();
        assert!(matches!(error, DriveError::Status { status: code, .. } if code == status));
    }
}

#[tokio::test]
async fn retry_after_pauses_the_whole_lane() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    let seen = arrivals.clone();
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(move |_: &Request| {
            let mut seen = seen.lock().unwrap();
            seen.push(Instant::now());
            if seen.len() == 1 {
                failure(429, "").insert_header("Retry-After", "1")
            } else {
                uploaded()
            }
        })
        .mount(&server)
        .await;
    export_and_delete(&server).await;
    let mut cfg = config(&server, &["one"]);
    cfg.concurrency_per_account = 1;
    cfg.max_attempts = 1;
    let recognizer = DriveRecognizer::new(cfg).await.unwrap();
    let mut tasks = JoinSet::new();
    for _ in 0..3 {
        let r = recognizer.clone();
        tasks.spawn(async move { r.recognize_page(&page()).await.unwrap() });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    let arrivals = arrivals.lock().unwrap();
    assert_eq!(arrivals.len(), 4);
    assert!(
        arrivals[1..]
            .iter()
            .all(|at| at.duration_since(arrivals[0]) >= Duration::from_millis(990))
    );
}

#[tokio::test]
async fn request_timeouts_are_retried() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(uploaded().set_delay(Duration::from_secs(2)))
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(uploaded())
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    export_and_delete(&server).await;
    let mut cfg = config(&server, &["one"]);
    cfg.request_timeout = Duration::from_millis(100);
    DriveRecognizer::new(cfg)
        .await
        .unwrap()
        .recognize_page(&page())
        .await
        .unwrap();
}

#[tokio::test]
async fn connection_failures_are_classified_for_retry() {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    let mut cfg = config(&server, &["one"]);
    cfg.token_url = format!("http://{address}/token");
    cfg.max_attempts = 2;
    let err = DriveRecognizer::new(cfg).await.unwrap_err();
    assert!(matches!(err, DriveError::Request(_)));
    assert!(err.is_retryable());
}

#[tokio::test]
async fn storage_quota_disables_only_that_service_account() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(|request: &Request| {
            let form = reqwest::Url::parse(&format!(
                "http://local/?{}",
                String::from_utf8_lossy(&request.body)
            ))
            .unwrap();
            let assertion = form
                .query_pairs()
                .find(|(key, _)| key == "assertion")
                .unwrap()
                .1
                .into_owned();
            let payload = URL_SAFE_NO_PAD
                .decode(assertion.split('.').nth(1).unwrap())
                .unwrap();
            let claims: serde_json::Value = serde_json::from_slice(&payload).unwrap();
            token(claims["iss"].as_str().unwrap(), 3600)
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(header("authorization", "Bearer no-space"))
        .respond_with(failure(403, "storageQuotaExceeded"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(header("authorization", "Bearer available"))
        .respond_with(uploaded())
        .expect(2)
        .mount(&server)
        .await;
    export_and_delete(&server).await;
    let r = DriveRecognizer::new(config(&server, &["no-space", "available"]))
        .await
        .unwrap();
    for _ in 0..2 {
        r.recognize_page(&page()).await.unwrap();
    }
}

#[tokio::test]
async fn no_accounts_is_a_run_level_error_and_stays_disabled() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(failure(403, "storageQuotaExceeded"))
        .expect(2)
        .mount(&server)
        .await;
    let r = DriveRecognizer::new(config(&server, &["one", "two"]))
        .await
        .unwrap();
    for _ in 0..2 {
        let err = r.recognize_page(&page()).await.unwrap_err();
        assert!(matches!(err, DriveError::NoAccounts));
        assert!(err.is_run_level());
    }
}

#[tokio::test]
async fn concurrent_pages_leave_a_disabled_lane_before_retrying_uploads() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    let count = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(move |_: &Request| {
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503).set_delay(Duration::from_millis(200))
            } else {
                failure(403, "storageQuotaExceeded")
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let recognizer = DriveRecognizer::new(config(&server, &["one"]))
        .await
        .unwrap();
    let image = page();
    let (first, second) = tokio::join!(
        recognizer.recognize_page(&image),
        recognizer.recognize_page(&image),
    );
    assert!(matches!(first, Err(DriveError::NoAccounts)));
    assert!(matches!(second, Err(DriveError::NoAccounts)));
}

#[tokio::test]
async fn export_failure_still_deletes_with_its_own_retries() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(uploaded())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/doc/export"))
        .respond_with(ResponseTemplate::new(503))
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = config(&server, &["one"]);
    cfg.max_attempts = 3;
    let err = DriveRecognizer::new(cfg)
        .await
        .unwrap()
        .recognize_page(&page())
        .await
        .unwrap_err();
    assert!(matches!(err, DriveError::Status { status: 503, .. }));
}

#[tokio::test]
async fn dropping_a_recognition_deletes_and_releases_capacity() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    let exporting = Arc::new(Notify::new());
    let notify = exporting.clone();
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(uploaded())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/doc/export"))
        .respond_with(move |_: &Request| {
            notify.notify_one();
            ResponseTemplate::new(200).set_delay(Duration::from_secs(30))
        })
        .mount(&server)
        .await;
    let deleted = Arc::new(Notify::new());
    let notify = deleted.clone();
    Mock::given(method("DELETE"))
        .respond_with(move |_: &Request| {
            notify.notify_one();
            ResponseTemplate::new(204)
        })
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = config(&server, &["one"]);
    cfg.concurrency_per_account = 1;
    let r = DriveRecognizer::new(cfg).await.unwrap();
    let task = tokio::spawn(async move { r.recognize_page(&page()).await });
    tokio::time::timeout(Duration::from_secs(3), exporting.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), deleted.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn cached_tokens_and_concurrent_401s_only_refresh_once() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token("old", 3600))
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token("new", 3600).set_delay(Duration::from_millis(20)))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(401).set_delay(Duration::from_millis(30)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(header("authorization", "Bearer new"))
        .respond_with(uploaded())
        .expect(12)
        .mount(&server)
        .await;
    export_and_delete(&server).await;
    let r = DriveRecognizer::new(config(&server, &["one"]))
        .await
        .unwrap();
    let mut tasks = JoinSet::new();
    for _ in 0..12 {
        let r = r.clone();
        tasks.spawn(async move { r.recognize_page(&page()).await.unwrap() });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
}

#[tokio::test]
async fn expired_tokens_have_one_refresh_for_many_pages() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token("access", 1))
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token("access", 3600))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    success(&server).await;
    let r = DriveRecognizer::new(config(&server, &["one"]))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1050)).await;
    let mut tasks = JoinSet::new();
    for _ in 0..12 {
        let r = r.clone();
        tasks.spawn(async move { r.recognize_page(&page()).await.unwrap() });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
}

#[tokio::test]
async fn a_second_401_stops_new_pages() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(token("bad", 3600))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(ResponseTemplate::new(401))
        .expect(2)
        .mount(&server)
        .await;
    let r = DriveRecognizer::new(config(&server, &["one"]))
        .await
        .unwrap();
    let err = r.recognize_page(&page()).await.unwrap_err();
    assert!(matches!(err, DriveError::Authentication(_)));
    assert!(err.is_run_level());
    assert!(matches!(
        r.recognize_page(&page()).await,
        Err(DriveError::CredentialsRejected)
    ));
}

#[tokio::test]
async fn invalid_credentials_fail_before_pages_and_are_redacted() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string("a-token-that-must-not-be-logged"))
        .expect(1)
        .mount(&server)
        .await;
    let cfg = config(&server, &["one"]);
    let debug = format!("{cfg:?}");
    assert!(debug.contains("[redacted]"));
    assert!(!debug.contains("PRIVATE KEY"));
    let err = DriveRecognizer::new(cfg).await.unwrap_err();
    assert!(err.is_run_level());
    assert!(!format!("{err:?}").contains("a-token-that"));
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "{bad-json}").unwrap();
    assert!(matches!(
        ServiceAccountKey::from_file(file.path()),
        Err(DriveError::InvalidKey(_))
    ));
}

#[tokio::test]
async fn validates_config_and_rejects_oversized_images_before_upload() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    assert!(
        DriveRecognizer::new(DriveConfig::new(DriveAuth::ServiceAccounts(vec![])))
            .await
            .is_err()
    );
    for invalid in 0..5 {
        let mut cfg = config(&server, &["one"]);
        match invalid {
            0 => cfg.max_attempts = 0,
            1 => cfg.concurrency_per_account = 0,
            2 => cfg.token_url = "not a url".into(),
            3 => cfg.request_timeout = Duration::ZERO,
            _ => cfg.language = Some(" ".into()),
        }
        assert!(DriveRecognizer::new(cfg).await.is_err());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    tokens(&server).await;
    let mut cfg = config(&server, &["one"]);
    cfg.max_image_bytes = 1;
    let r = DriveRecognizer::new(cfg).await.unwrap();
    assert!(matches!(
        r.recognize_page(&page()).await,
        Err(DriveError::ImageTooLarge { .. })
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn png_uses_its_own_content_type_and_concurrency_is_bounded() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(move |request: &Request| {
            assert!(
                request
                    .body
                    .windows(23)
                    .any(|part| part == b"Content-Type: image/png")
            );
            seen.fetch_add(1, Ordering::SeqCst);
            uploaded().set_delay(Duration::from_millis(40))
        })
        .mount(&server)
        .await;
    export_and_delete(&server).await;
    let mut cfg = config(&server, &["one"]);
    cfg.concurrency_per_account = 1;
    let r = DriveRecognizer::new(cfg).await.unwrap();
    let mut tasks = JoinSet::new();
    for _ in 0..3 {
        let r = r.clone();
        tasks.spawn(async move {
            r.recognize_page(&PageImage::new(
                vec![137, 80, 78, 71],
                ImageFormat::Png,
                1,
                1,
            ))
            .await
            .unwrap()
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    assert_eq!(count.load(Ordering::SeqCst), 3);
    let requests = server.received_requests().await.unwrap();
    let methods: Vec<_> = requests.iter().skip(2).map(|r| r.method.as_str()).collect();
    assert_eq!(
        methods,
        [
            "POST", "GET", "DELETE", "POST", "GET", "DELETE", "POST", "GET", "DELETE"
        ]
    );
}

#[tokio::test]
async fn structured_status_rate_limit_is_understood_and_delete_failure_keeps_text() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(serde_json::json!({"error":{"status":"RESOURCE_EXHAUSTED"}})),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(uploaded())
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/doc/export"))
        .respond_with(ResponseTemplate::new(200).set_body_string("text"))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(503))
        .expect(2)
        .mount(&server)
        .await;
    let mut cfg = config(&server, &["one"]);
    cfg.max_attempts = 2;
    let text = DriveRecognizer::new(cfg)
        .await
        .unwrap()
        .recognize_page(&page())
        .await
        .unwrap()
        .text;
    assert_eq!(text, "text");
}

#[tokio::test]
async fn dropping_during_delete_retries_cleanup() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .respond_with(uploaded())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/doc/export"))
        .respond_with(ResponseTemplate::new(200).set_body_string("text"))
        .mount(&server)
        .await;
    let deleting = Arc::new(Notify::new());
    let notify = deleting.clone();
    Mock::given(method("DELETE"))
        .respond_with(move |_: &Request| {
            notify.notify_one();
            ResponseTemplate::new(204).set_delay(Duration::from_secs(30))
        })
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let deleted = Arc::new(Notify::new());
    let notify = deleted.clone();
    Mock::given(method("DELETE"))
        .respond_with(move |_: &Request| {
            notify.notify_one();
            ResponseTemplate::new(204)
        })
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let r = DriveRecognizer::new(config(&server, &["one"]))
        .await
        .unwrap();
    let task = tokio::spawn(async move { r.recognize_page(&page()).await });
    tokio::time::timeout(Duration::from_secs(3), deleting.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), deleted.notified())
        .await
        .unwrap();
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[tokio::test]
async fn startup_cleanup_filters_old_tagged_files_and_follows_all_pages() {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};

    let server = MockServer::start().await;
    tokens(&server).await;
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish(),
    );
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .and(query_param("fields", "nextPageToken,files(id)"))
        .and(header("authorization", "Bearer access"))
        .respond_with(|request: &Request| {
            let page = request
                .url
                .query_pairs()
                .find(|(key, _)| key == "pageToken");
            let body = match page.as_ref().map(|(_, value)| value.as_ref()) {
                None => {
                    serde_json::json!({"files": [{"id": "old-one"}], "nextPageToken": "second"})
                }
                Some("second") => serde_json::json!({"files": [], "nextPageToken": "third"}),
                Some("third") => serde_json::json!({"files": [{"id": "old-two"}]}),
                other => panic!("unexpected page token: {other:?}"),
            };
            ResponseTemplate::new(200).set_body_json(body)
        })
        .expect(3)
        .mount(&server)
        .await;
    for id in ["old-one", "old-two"] {
        Mock::given(method("DELETE"))
            .and(path(format!("/drive/v3/files/{id}")))
            .and(header("authorization", "Bearer access"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    let earliest = OffsetDateTime::now_utc() - time::Duration::hours(1);
    DriveRecognizer::new(config(&server, &["one"]))
        .await
        .unwrap();
    let latest = OffsetDateTime::now_utc() - time::Duration::hours(1);
    let requests = server.received_requests().await.unwrap();
    let queries: Vec<_> = requests
        .iter()
        .filter(|request| request.method == "GET")
        .map(|request| {
            request
                .url
                .query_pairs()
                .find(|(key, _)| key == "q")
                .unwrap()
                .1
                .into_owned()
        })
        .collect();
    assert_eq!(queries.len(), 3);
    assert!(queries.iter().all(|query| query == &queries[0]));
    let cutoff = queries[0]
        .strip_prefix("appProperties has { key='tahweel' and value='ocr' } and createdTime < '")
        .unwrap()
        .strip_suffix("' and trashed = false")
        .unwrap();
    let cutoff = OffsetDateTime::parse(cutoff, &Rfc3339).unwrap();
    assert!((earliest..=latest).contains(&cutoff));
    assert!(logs.text().contains("deleted leftover Google Drive files"));
    assert!(logs.text().contains("count=2"));
}

#[tokio::test]
async fn startup_cleans_each_account_with_its_own_token() {
    let server = MockServer::start().await;
    let count = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(move |_: &Request| {
            token(
                &format!("account-{}", count.fetch_add(1, Ordering::SeqCst)),
                3600,
            )
        })
        .expect(2)
        .mount(&server)
        .await;
    for index in 0..2 {
        Mock::given(method("GET"))
            .and(path("/drive/v3/files"))
            .and(header("authorization", format!("Bearer account-{index}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"files": [{"id": format!("old-{index}")}]})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/drive/v3/files/old-{index}")))
            .and(header("authorization", format!("Bearer account-{index}")))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    DriveRecognizer::new(config(&server, &["one", "two"]))
        .await
        .unwrap();
}

#[tokio::test]
async fn cleanup_list_failures_warn_and_do_not_stop_recognition() {
    for status in [200, 400, 401, 503] {
        let server = MockServer::start().await;
        tokens(&server).await;
        success(&server).await;
        let logs = LogBuffer::default();
        let writer = logs.clone();
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish(),
        );
        Mock::given(method("GET"))
            .and(path("/drive/v3/files"))
            .respond_with(ResponseTemplate::new(status).set_body_string("invalid list"))
            .expect(if matches!(status, 401 | 503) { 2 } else { 1 })
            .mount(&server)
            .await;
        let mut cfg = config(&server, &["one"]);
        cfg.max_attempts = 2;
        let recognizer = DriveRecognizer::new(cfg).await.unwrap();
        assert_eq!(
            recognizer.recognize_page(&page()).await.unwrap().text,
            "نص\nعربي"
        );
        assert!(logs.text().contains("WARN"));
        assert!(
            logs.text()
                .contains("could not list leftover Google Drive files")
        );
    }
}

#[tokio::test]
async fn cleanup_delete_failures_warn_and_continue_to_other_files_and_pages() {
    let server = MockServer::start().await;
    tokens(&server).await;
    success(&server).await;
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish(),
    );
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "files": [{"id": "failed"}, {"id": "already-gone"}, {"id": "retry"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/failed"))
        .respond_with(ResponseTemplate::new(503))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/already-gone"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/retry"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/retry"))
        .respond_with(ResponseTemplate::new(204))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = config(&server, &["one"]);
    cfg.max_attempts = 2;
    let recognizer = DriveRecognizer::new(cfg).await.unwrap();
    recognizer.recognize_page(&page()).await.unwrap();
    let logs = logs.text();
    assert!(logs.contains("could not delete temporary Google Drive file"));
    assert!(logs.contains(r#"file_id="failed""#));
    assert!(logs.contains("count=2"));
}

#[tokio::test]
async fn cleanup_refresh_failure_does_not_latch_rejected_credentials() {
    let server = MockServer::start().await;
    let count = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(move |_: &Request| {
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                token("access", 3600)
            } else {
                ResponseTemplate::new(400)
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    success(&server).await;
    DriveRecognizer::new(config(&server, &["one"]))
        .await
        .unwrap()
        .recognize_page(&page())
        .await
        .unwrap();
}

#[tokio::test]
async fn closed_connections_and_truncated_bodies_are_retried_for_every_operation() {
    for failure in [
        Vec::new(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{".to_vec(),
    ] {
        let (base, server) = transport::server(vec![
            failure.clone(),
            transport::response(
                200,
                r#"{"access_token":"access","expires_in":3600,"token_type":"Bearer"}"#,
            ),
            failure.clone(),
            transport::response(200, r#"{"files":[]}"#),
            failure.clone(),
            transport::response(200, r#"{"id":"doc"}"#),
            failure.clone(),
            transport::response(200, "نص"),
            failure,
            transport::response(204, ""),
        ])
        .await;
        let unused = MockServer::start().await;
        let mut cfg = config(&unused, &["one"]);
        cfg.api_base.clone_from(&base);
        cfg.upload_base = format!("{base}/upload");
        cfg.token_url = format!("{base}/token");
        cfg.max_attempts = 2;
        let recognizer = DriveRecognizer::new(cfg).await.unwrap();
        assert_eq!(recognizer.recognize_page(&page()).await.unwrap().text, "نص");
        let requests = server.await.unwrap();
        let methods: Vec<_> = requests
            .iter()
            .map(|line| line.split(' ').next().unwrap())
            .collect();
        assert_eq!(
            methods,
            [
                "POST", "POST", "GET", "GET", "POST", "POST", "GET", "GET", "DELETE", "DELETE"
            ]
        );
    }
}

#[tokio::test]
async fn transport_retries_stop_at_the_attempt_limit() {
    for failure in [
        Vec::new(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{".to_vec(),
    ] {
        let (base, server) = transport::server(vec![failure; 3]).await;
        let unused = MockServer::start().await;
        let mut cfg = config(&unused, &["one"]);
        cfg.token_url = format!("{base}/token");
        cfg.max_attempts = 3;
        let error = DriveRecognizer::new(cfg).await.unwrap_err();
        assert!(error.is_retryable());
        let DriveError::Request(error) = error else {
            panic!("expected a transport error")
        };
        assert!(!error.is_connect());
        assert!(!error.is_timeout());
        assert_eq!(server.await.unwrap().len(), 3);
    }
}
