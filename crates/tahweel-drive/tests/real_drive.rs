//! Opt-in test: one page, using the key path explicitly supplied by the test runner.

use tahweel::{ImageFormat, PageImage};
use tahweel_drive::{DriveAuth, DriveConfig, DriveError, DriveRecognizer, ServiceAccountKey};

#[tokio::test]
#[ignore = "requires TAHWEEL_TEST_SERVICE_ACCOUNT and deletes old tagged Drive files"]
async fn initializes_and_cleans_up_leftovers() {
    let path = std::env::var_os("TAHWEEL_TEST_SERVICE_ACCOUNT")
        .expect("set TAHWEEL_TEST_SERVICE_ACCOUNT to a key file");
    let key = ServiceAccountKey::from_file(path).unwrap();
    let mut config = DriveConfig::new(DriveAuth::ServiceAccounts(vec![key]));
    config.concurrency_per_account = 1;
    let _subscriber = tracing::subscriber::set_default(tracing_subscriber::fmt().finish());
    DriveRecognizer::new(config).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TAHWEEL_TEST_SERVICE_ACCOUNT and makes real Drive requests"]
async fn recognizes_arabic_fixture() {
    let path = std::env::var_os("TAHWEEL_TEST_SERVICE_ACCOUNT")
        .expect("set TAHWEEL_TEST_SERVICE_ACCOUNT to a key file");
    let key = ServiceAccountKey::from_file(path).unwrap();
    let mut config = DriveConfig::new(DriveAuth::ServiceAccounts(vec![key]));
    config.concurrency_per_account = 1;
    let recognizer = DriveRecognizer::new(config).await.unwrap();
    let page = PageImage::new(
        include_bytes!("fixtures/test-case.jpg").to_vec(),
        ImageFormat::Jpeg,
        1241,
        1754,
    );
    let start = std::time::Instant::now();
    let text = recognizer.recognize_page(&page).await.unwrap().text;
    println!("OCR completed in {:?}:\n{text}", start.elapsed());
    assert!(text.contains("بسم"));
    assert!(text.contains("الله"));
    assert!(!text.contains('\r'));
    assert!(!text.starts_with('\u{feff}'));
    assert!(!text.starts_with("________________"));

    // Optional second account, explicitly supplied only when checking the storage failure.
    if let Some(path) = std::env::var_os("TAHWEEL_TEST_NO_STORAGE_SERVICE_ACCOUNT") {
        let key = ServiceAccountKey::from_file(path).unwrap();
        let mut config = DriveConfig::new(DriveAuth::ServiceAccounts(vec![key]));
        config.concurrency_per_account = 1;
        let recognizer = DriveRecognizer::new(config).await.unwrap();
        let error = recognizer.recognize_page(&page).await.unwrap_err();
        assert!(matches!(error, DriveError::NoAccounts));
        assert!(error.is_run_level());
        println!("Account without storage correctly returned NoAccounts");
    }
}
