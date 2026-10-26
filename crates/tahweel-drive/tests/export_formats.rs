//! Raw export comparisons use local Drive mocks and never contact Google.

mod common;

use std::{sync::Arc, time::Duration};

use common::{config, failure, no_leftovers, page, success, tokens, uploaded};
use tahweel_drive::{DriveError, DriveRecognizer};
use tokio::sync::Notify;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

#[tokio::test]
async fn exports_formats_from_one_document_without_changing_the_bytes() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(query_param("ocrLanguage", "ar"))
        .and(header("authorization", "Bearer access"))
        .respond_with(uploaded())
        .expect(1)
        .mount(&server)
        .await;

    let plain = "\u{feff}________________\r\n\r\n نص عربي \r\n".as_bytes();
    let html = "<p dir=\"rtl\">نص عربي</p>\r\n".as_bytes();
    let docx = &[0x50, 0x4b, 0x03, 0x04, 0xff, 0x00, 0x81];
    let zip = &[0x50, 0x4b, 0x03, 0x04, 0xfe, 0x00, 0x82];
    let formats = ["text/plain", "text/html", DOCX, "application/zip"];
    let bodies: [&[u8]; 4] = [plain, html, docx, zip];
    for (mime_type, body) in formats.iter().zip(bodies) {
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/doc/export"))
            .and(query_param("mimeType", *mime_type))
            .and(header("authorization", "Bearer access"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/doc"))
        .and(header("authorization", "Bearer access"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let recognizer = DriveRecognizer::new(config(&server, &["one"]))
        .await
        .unwrap();
    let exports = recognizer
        .export_page_formats(&page(), &formats)
        .await
        .unwrap();
    assert_eq!(exports.len(), bodies.len());
    for (export, expected) in exports.iter().zip(bodies) {
        assert_eq!(export.as_ref().unwrap().as_ref(), expected);
    }

    let requests = server.received_requests().await.unwrap();
    // Initial token and leftover checks precede the single upload and four exports.
    assert_eq!(requests.len(), 8);
    assert_eq!(requests[2].method, "POST");
    for (request, mime_type) in requests[3..7].iter().zip(formats) {
        assert_eq!(request.method, "GET");
        assert_eq!(request.url.path(), "/drive/v3/files/doc/export");
        assert_eq!(
            request
                .url
                .query_pairs()
                .find(|(key, _)| key == "mimeType")
                .map(|(_, value)| value.into_owned())
                .as_deref(),
            Some(mime_type)
        );
    }
    assert_eq!(requests[7].method, "DELETE");
    assert_eq!(requests[7].url.path(), "/drive/v3/files/doc");
}

#[tokio::test]
async fn failed_format_retries_then_preserves_other_exports_and_deletes() {
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
        .and(query_param("mimeType", "text/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_string("نص\r\n"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/doc/export"))
        .and(query_param("mimeType", "text/html"))
        .respond_with(failure(503, "backendError"))
        .expect(3)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/doc/export"))
        .and(query_param("mimeType", DOCX))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0xff, 0x00, 0x81]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/doc"))
        .respond_with(failure(503, "backendError"))
        .up_to_n_times(1)
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/doc"))
        .respond_with(ResponseTemplate::new(204))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;

    let mut cfg = config(&server, &["one"]);
    cfg.max_attempts = 3;
    let recognizer = DriveRecognizer::new(cfg).await.unwrap();
    let exports = recognizer
        .export_page_formats(&page(), &["text/plain", "text/html", DOCX])
        .await
        .unwrap();
    assert_eq!(exports.len(), 3);
    assert_eq!(exports[0].as_ref().unwrap().as_ref(), "نص\r\n".as_bytes());
    assert!(matches!(
        &exports[1],
        Err(DriveError::Status { status: 503, .. })
    ));
    assert_eq!(exports[2].as_ref().unwrap().as_ref(), &[0xff, 0x00, 0x81]);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.last().unwrap().method, "DELETE");
}

#[tokio::test]
async fn rejects_empty_or_blank_formats_before_uploading() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    let recognizer = DriveRecognizer::new(config(&server, &["one"]))
        .await
        .unwrap();

    let invalid: &[&[&str]] = &[&[], &[""], &["text/plain", " \t "]];
    for formats in invalid {
        let error = recognizer
            .export_page_formats(&page(), formats)
            .await
            .unwrap_err();
        assert!(matches!(error, DriveError::Config(_)));
    }
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        2,
        "only initialization should contact Drive"
    );
}

#[tokio::test]
async fn cancelling_a_later_export_deletes_and_releases_the_account() {
    let server = MockServer::start().await;
    no_leftovers(&server).await;
    tokens(&server).await;
    success(&server).await;

    let exporting = Arc::new(Notify::new());
    let notify = exporting.clone();
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/doc/export"))
        .and(query_param("mimeType", "text/html"))
        .respond_with(move |_: &Request| {
            notify.notify_one();
            ResponseTemplate::new(200).set_delay(Duration::from_secs(30))
        })
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let deleted = Arc::new(Notify::new());
    let notify = deleted.clone();
    Mock::given(method("DELETE"))
        .and(path("/drive/v3/files/doc"))
        .respond_with(move |_: &Request| {
            notify.notify_one();
            ResponseTemplate::new(204)
        })
        .with_priority(1)
        .expect(2)
        .mount(&server)
        .await;

    let mut cfg = config(&server, &["one"]);
    cfg.concurrency_per_account = 1;
    let recognizer = DriveRecognizer::new(cfg).await.unwrap();
    let first = recognizer.clone();
    let task = tokio::spawn(async move {
        first
            .export_page_formats(&page(), &["text/plain", "text/html"])
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), exporting.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), deleted.notified())
        .await
        .unwrap();

    // A normal recognition can acquire the one available account after cleanup.
    let image = page();
    let result = tokio::time::timeout(Duration::from_secs(3), recognizer.recognize_page(&image))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.text, "نص\nعربي");
}
