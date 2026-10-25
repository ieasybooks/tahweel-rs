//! Public CLI behavior, using only generated fixtures and local Drive mocks.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use aws_lc_rs::{
    encoding::AsDer,
    rsa::{KeyPair, KeySize},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

#[path = "../crates/tahweel/tests/support/mod.rs"]
mod fixtures;

const TEXT: &str = "نص عربي\nمن الكتاب";

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tahweel"));
    for key in [
        "TAHWEEL_SERVICE_ACCOUNTS",
        "TAHWEEL_PDFIUM",
        "TAHWEEL_TEST_SERVICE_ACCOUNT",
        "RUST_LOG",
        "CLICOLOR_FORCE",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env_remove(key);
    }
    command.env("NO_COLOR", "1").env("NO_PROXY", "*");
    command
}

fn isolated_command(dir: &Path) -> Command {
    let mut command = command();
    // Isolate user directories and shell preferences for every subprocess.
    command
        .current_dir(dir)
        .env("HOME", dir)
        .env("USERPROFILE", dir)
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_CONFIG_HOME", dir.join("config"));
    #[cfg(windows)]
    {
        // dirs uses SHGetKnownFolderPath, which verifies these folders exist under
        // USERPROFILE. Setting LOCALAPPDATA and APPDATA alone does not create them.
        let local = dir.join("AppData").join("Local");
        let roaming = dir.join("AppData").join("Roaming");
        fs::create_dir_all(&local).unwrap();
        fs::create_dir_all(&roaming).unwrap();
        command.env("LOCALAPPDATA", local).env("APPDATA", roaming);
    }
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn success(output: &Output) {
    assert!(output.status.success(), "{}", stderr(output));
    assert!(output.stdout.is_empty(), "{}", stdout(output));
}

async fn run(mut command: Command) -> Output {
    tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap()
}

#[test]
fn help_version_and_spec_work_without_credentials_or_tools() {
    for args in [
        vec!["--help"],
        vec!["convert", "--help"],
        vec!["--version"],
        vec!["spec"],
        vec!["__usage_spec__"],
    ] {
        let output = command().env("PATH", "").args(&args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {}", stderr(&output));
        assert_ne!(stdout(&output), "");
        assert!(output.stderr.is_empty(), "{}", stderr(&output));
    }
    let help = stdout(&command().arg("--help").output().unwrap());
    for text in [
        "convert",
        "completions",
        "spec",
        "Output",
        "Pages",
        "Google Drive",
        "@FILE",
        "--pdfium",
        "--drive-token",
        "google-token.json",
        "sign out",
    ] {
        assert!(help.contains(text), "missing {text}: {help}");
    }
    assert!(!help.contains("--drive-test-url"));
    let spec = stdout(&command().arg("spec").output().unwrap());
    assert_eq!(
        spec,
        stdout(&command().arg("__usage_spec__").output().unwrap())
    );
    for code in [
        "exit_code 0",
        "exit_code 1",
        "exit_code 2",
        "exit_code 3",
        "exit_code 130",
    ] {
        assert!(spec.contains(code), "missing {code}: {spec}");
    }
}

#[test]
fn help_and_spec_do_not_embed_service_account_environment_values() {
    for args in [vec!["--help"], vec!["convert", "--help"], vec!["spec"]] {
        let output = command()
            .env("TAHWEEL_SERVICE_ACCOUNTS", "user-credential-path.json")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        assert!(!stdout(&output).contains("user-credential-path.json"));
        assert!(!stderr(&output).contains("user-credential-path.json"));
    }
}

#[test]
fn inputs_recognizer_and_service_account_are_required() {
    for args in [
        vec![],
        vec!["convert"],
        vec!["book.pdf"],
        vec!["book.pdf", "--recognizer", "drive-service-account"],
        vec![
            "book.pdf",
            "--recognizer",
            "drive-service-account",
            "--service-account",
            "",
        ],
        vec!["book.pdf", "--recognizer", "other"],
    ] {
        let output = command().env("PATH", "").args(&args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn invalid_arguments_fail_before_credentials_inputs_or_pdfium_are_loaded() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["--dpi", "0"],
        vec!["--dpi", "-1"],
        vec!["--dpi", "NaN"],
        vec!["--drive-concurrency", "0"],
        vec!["--drive-concurrency", "18446744073709551615"],
        vec!["--drive-attempts", "0"],
        vec!["--language", " "],
        vec!["--output-mode", "flat"],
        vec!["--output-mode", "tree-to-tree"],
        vec!["--output-mode", "side-by-side", "-o", "out"],
        vec!["--format", "srt"],
        vec!["--output-dir", ""],
        vec!["--pdfium", ""],
        vec!["--drive-test-url", "not-a-url"],
        vec!["--drive-test-url", "https://example.test/?x=y"],
        vec!["--typo"],
    ] {
        let output = isolated_command(dir.path())
            .env("PATH", "")
            .args([
                "convert",
                "missing.pdf",
                "--recognizer",
                "drive-service-account",
                "--service-account",
                "missing.json",
            ])
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert_eq!(stdout(&output), "");
        assert!(stderr(&output).contains("error:"));
        assert!(!stderr(&output).contains("could not read"));
        assert!(!dir.path().join("out").exists());
    }
}

#[test]
fn recognizer_specific_options_are_rejected_including_from_response_files() {
    let dir = tempfile::tempdir().unwrap();
    for (backend, flag, value) in [
        ("drive-oauth", "--service-account", "missing.json"),
        (
            "drive-service-account",
            "--drive-token",
            "missing-token.json",
        ),
    ] {
        let preset = format!("book.pdf --recognizer {backend} {flag} {value}");
        fs::write(dir.path().join("preset.args"), &preset).unwrap();
        for from_file in [false, true] {
            let mut cmd = isolated_command(dir.path());
            if from_file {
                cmd.arg("@preset.args");
            } else {
                cmd.args(preset.split_whitespace());
            }
            let output = cmd.output().unwrap();
            assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
            assert!(stderr(&output).contains(flag), "{}", stderr(&output));
            assert!(
                stderr(&output).contains("isn't used by"),
                "{}",
                stderr(&output)
            );
        }
    }
}

#[test]
fn completion_scripts_and_protocol_work_with_empty_path() {
    for shell in ["bash", "elvish", "fish", "nu", "powershell", "zsh"] {
        let output = command()
            .env("PATH", "")
            .args(["completions", shell])
            .output()
            .unwrap();
        assert!(output.status.success(), "{shell}: {}", stderr(&output));
        assert!(stdout(&output).contains("__complete_word__"));
        assert_eq!(stderr(&output), "");
    }
    let output = command()
        .env("PATH", "")
        .args([
            "__complete_word__",
            "--shell",
            "bash",
            "--words",
            "tahweel",
            "convert",
            "@does-not-exist.args",
            "--output-mode",
            "tr",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("tree-to-tree"));
    for args in [
        vec!["completions", "bogus"],
        vec!["completions", "fish", "--dry-run"],
    ] {
        assert_eq!(
            command().args(args).output().unwrap().status.code(),
            Some(2)
        );
    }
}

#[test]
fn completion_installation_previews_updates_and_preserves_foreign_files() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir
        .path()
        .join("config")
        .join("fish")
        .join("completions")
        .join("tahweel.fish");
    let install = |preview: bool| {
        let mut cmd = isolated_command(dir.path());
        cmd.env("PATH", "")
            .args(["completions", "fish", "--install"]);
        if preview {
            cmd.arg("--dry-run");
        }
        cmd.output().unwrap()
    };
    let output = install(true);
    assert!(output.status.success(), "{}", stderr(&output));
    let preview = stdout(&output);
    let destination = preview
        .lines()
        .next()
        .unwrap()
        .strip_prefix("Would install completions to ")
        .expect("dry run must print its destination");
    assert_eq!(Path::new(destination), target);
    assert!(!target.exists());
    let output = install(false);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .contains("__complete_word__")
    );
    assert!(stdout(&install(false)).contains("Already installed"));
    fs::write(&target, "# user maintained\n").unwrap();
    assert_eq!(install(false).status.code(), Some(1));
    assert_eq!(fs::read_to_string(target).unwrap(), "# user maintained\n");
}

#[test]
fn response_files_report_cycles_missing_files_and_bad_quoting() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("cycle.args"), "@cycle.args").unwrap();
    fs::write(dir.path().join("broken.args"), "'unclosed").unwrap();
    for file in ["cycle.args", "missing.args", "broken.args"] {
        let output = isolated_command(dir.path())
            .arg(format!("@{file}"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(stderr(&output).contains(file), "{}", stderr(&output));
    }
}

fn write_key(path: &Path) {
    static KEY: OnceLock<KeyPair> = OnceLock::new();
    let key = KEY.get_or_init(|| KeyPair::generate(KeySize::Rsa2048).unwrap());
    let der = key.as_der().unwrap();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        STANDARD.encode(der.as_ref())
    );
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "type": "service_account", "client_email": "cli@example.test", "private_key": pem,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_image(path: &Path) {
    image::RgbImage::from_pixel(32, 24, image::Rgb([240, 230, 220]))
        .save(path)
        .unwrap();
}

async fn mock_drive() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token":"mock-access", "expires_in":3600, "token_type":"Bearer"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files":[]})))
        .mount(&server)
        .await;
    let next = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/upload/drive/v3/files"))
        .and(query_param("uploadType", "multipart"))
        .and(header("authorization", "Bearer mock-access"))
        .respond_with(move |_: &Request| {
            let id = next.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({"id":format!("page-{id}")}))
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("mimeType", "text/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "\u{feff}________________\r\n\r\n{}\r\n",
            TEXT.replace('\n', "\r\n")
        )))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    server
}

fn service_command(dir: &Path, server: &MockServer) -> Command {
    let mut command = isolated_command(dir);
    command.args([
        "--recognizer",
        "drive-service-account",
        "--service-account",
        "key.json",
        "--drive-test-url",
        &server.uri(),
        "-q",
    ]);
    command
}

async fn assert_round_trips(server: &MockServer, count: usize) {
    let requests = server.received_requests().await.unwrap();
    let uploads = requests
        .iter()
        .filter(|r| r.url.path() == "/upload/drive/v3/files")
        .count();
    let deletes = requests.iter().filter(|r| r.method == "DELETE").count();
    assert_eq!(uploads, count);
    assert_eq!(deletes, count);
}

fn assert_outputs(dir: &Path, name: &str, pages: usize, separator: &str) {
    assert_eq!(
        fs::read_to_string(dir.join(format!("{name}.txt"))).unwrap(),
        vec![TEXT; pages].join(separator)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join(format!("{name}.json"))).unwrap()).unwrap();
    assert_eq!(
        json,
        serde_json::Value::Array(
            (1..=pages)
                .map(|page| json!({"page":page,"content":TEXT}))
                .collect()
        )
    );
    let mut archive =
        zip::ZipArchive::new(fs::File::open(dir.join(format!("{name}.docx"))).unwrap()).unwrap();
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    let doc = roxmltree::Document::parse(&xml).unwrap();
    let paragraphs = doc
        .descendants()
        .filter(|node| {
            node.has_tag_name((
                "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
                "p",
            ))
        })
        .filter(|node| {
            node.descendants().any(|child| {
                child.has_tag_name((
                    "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
                    "t",
                ))
            })
        })
        .count();
    assert_eq!(paragraphs, pages);
    for line in TEXT.lines() {
        assert!(xml.contains(line), "{xml}");
    }
}

#[tokio::test]
async fn converts_images_to_all_formats_in_every_output_mode() {
    for mode in ["side-by-side", "tree-to-tree", "flat"] {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("books/nested");
        fs::create_dir_all(&nested).unwrap();
        write_image(&dir.path().join("books/first.png"));
        write_image(&nested.join("second.jpg"));
        write_image(&nested.join("third.JPEG"));
        write_key(&dir.path().join("key.json"));
        let server = mock_drive().await;
        let mut cmd = service_command(dir.path(), &server);
        cmd.args([
            "books",
            "-r",
            "--pdfium",
            "missing-library",
            "--output-mode",
            mode,
        ]);
        if mode != "side-by-side" {
            cmd.args(["-o", "out"]);
        }
        success(&run(cmd).await);
        let root = if mode == "side-by-side" {
            dir.path().join("books")
        } else {
            dir.path().join("out")
        };
        let nested = if mode == "flat" {
            root.clone()
        } else {
            root.join("nested")
        };
        assert_outputs(&root, "first", 1, "\n\nPAGE_SEPARATOR\n\n");
        assert_outputs(&nested, "second", 1, "\n\nPAGE_SEPARATOR\n\n");
        assert_outputs(&nested, "third", 1, "\n\nPAGE_SEPARATOR\n\n");
        assert_round_trips(&server, 3).await;
    }
}

#[tokio::test]
async fn converts_text_layer_pdfs_to_all_formats_in_every_output_mode() {
    let Some(pdfium) = fixtures::pdfium_library() else {
        return;
    };
    for mode in ["side-by-side", "tree-to-tree", "flat"] {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("books/nested")).unwrap();
        fixtures::pdf(&dir.path().join("books/nested/book.pdf"), &[20, 30]);
        write_key(&dir.path().join("key.json"));
        let server = mock_drive().await;
        let mut cmd = service_command(dir.path(), &server);
        cmd.args(["books", "-r", "--dpi", "72", "--output-mode", mode]);
        if mode == "side-by-side" {
            cmd.env("TAHWEEL_PDFIUM", &pdfium);
        } else {
            cmd.env("TAHWEEL_PDFIUM", "wrong-library")
                .arg("--pdfium")
                .arg(&pdfium);
            cmd.args(["-o", "out", "--page-separator", r"\n\t\\n\n"]);
        }
        success(&run(cmd).await);
        let output = match mode {
            "side-by-side" => dir.path().join("books/nested"),
            "tree-to-tree" => dir.path().join("out/nested"),
            _ => dir.path().join("out"),
        };
        assert_outputs(
            &output,
            "book",
            2,
            if mode == "side-by-side" {
                "\n\nPAGE_SEPARATOR\n\n"
            } else {
                "\n\t\\n\n"
            },
        );
        assert_round_trips(&server, 2).await;
    }
}

#[tokio::test]
async fn presets_environment_keys_negations_and_default_output_layout_work() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("books/nested")).unwrap();
    fs::create_dir(dir.path().join("presets")).unwrap();
    write_image(&dir.path().join("books/page.png"));
    write_image(&dir.path().join("books/nested/skipped.png"));
    write_key(&dir.path().join("key.json"));
    fs::write(
        dir.path().join("presets/base.args"),
        "--recognizer drive-service-account -r -q --skip-existing -l en -f txt -o out",
    )
    .unwrap();
    fs::write(dir.path().join("preset.args"), "@presets/base.args books").unwrap();
    let server = mock_drive().await;
    let mut cmd = isolated_command(dir.path());
    cmd.env("TAHWEEL_SERVICE_ACCOUNTS", "key.json,key.json")
        .args([
            "@preset.args",
            "--no-recursive",
            "--no-quiet",
            "--no-skip-existing",
            "-vv",
            "--drive-test-url",
            &server.uri(),
        ]);
    success(&run(cmd).await);
    assert_eq!(
        fs::read_to_string(dir.path().join("out/page.txt")).unwrap(),
        TEXT
    );
    assert!(!dir.path().join("out/page.json").exists());
    assert!(!dir.path().join("out/nested").exists());
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests.iter().filter(|r| r.url.path() == "/token").count(),
        2
    );
    let upload = requests
        .iter()
        .find(|r| r.url.path() == "/upload/drive/v3/files")
        .unwrap();
    assert!(
        upload
            .url
            .query_pairs()
            .any(|(key, value)| key == "ocrLanguage" && value == "en")
    );
    assert_round_trips(&server, 1).await;
}

#[tokio::test]
async fn repeated_and_comma_separated_keys_override_environment() {
    let dir = tempfile::tempdir().unwrap();
    write_image(&dir.path().join("page.png"));
    write_key(&dir.path().join("key.json"));
    let server = mock_drive().await;
    let mut cmd = isolated_command(dir.path());
    cmd.env("TAHWEEL_SERVICE_ACCOUNTS", "missing-key.json")
        .args([
            "page.png",
            "--recognizer",
            "drive-service-account",
            "--service-account",
            "key.json,key.json",
            "--service-account",
            "key.json",
            "--drive-test-url",
            &server.uri(),
            "-q",
        ]);
    success(&run(cmd).await);
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/token")
            .count(),
        3
    );
    assert_round_trips(&server, 1).await;
}

#[tokio::test]
async fn skip_existing_requires_every_requested_format() {
    let dir = tempfile::tempdir().unwrap();
    write_image(&dir.path().join("page.png"));
    write_key(&dir.path().join("key.json"));
    let server = mock_drive().await;
    let invoke = || {
        let mut cmd = service_command(dir.path(), &server);
        cmd.args(["page.png", "--skip-existing"]);
        cmd
    };
    success(&run(invoke()).await);
    fs::write(dir.path().join("page.txt"), "kept").unwrap();
    let skipped = run(invoke()).await;
    success(&skipped);
    assert!(stderr(&skipped).contains("1 skipped"));
    assert_eq!(
        fs::read_to_string(dir.path().join("page.txt")).unwrap(),
        "kept"
    );
    assert_round_trips(&server, 1).await;
    fs::remove_file(dir.path().join("page.json")).unwrap();
    success(&run(invoke()).await);
    assert_outputs(dir.path(), "page", 1, "");
    assert_round_trips(&server, 2).await;
}

#[tokio::test]
async fn a_failed_page_returns_three_and_other_documents_are_written() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["first.png", "second.png"] {
        write_image(&dir.path().join(name));
    }
    write_key(&dir.path().join("key.json"));
    let server = mock_drive().await;
    Mock::given(method("GET"))
        .and(path("/drive/v3/files/page-0/export"))
        .respond_with(ResponseTemplate::new(503))
        .with_priority(1)
        .mount(&server)
        .await;
    let mut cmd = service_command(dir.path(), &server);
    cmd.args([
        "first.png",
        "second.png",
        "--drive-concurrency",
        "1",
        "--drive-attempts",
        "1",
    ]);
    let output = run(cmd).await;
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(stderr(&output).contains("1 converted, 1 failed"));
    for ext in ["txt", "docx", "json"] {
        assert!(!dir.path().join(format!("first.{ext}")).exists());
    }
    assert_outputs(dir.path(), "second", 1, "");
    assert_round_trips(&server, 2).await;
    let mut retry = service_command(dir.path(), &server);
    retry.arg("first.png");
    success(&run(retry).await);
    assert_outputs(dir.path(), "first", 1, "");
}

#[tokio::test]
async fn oauth_refreshes_a_saved_token_and_ignores_service_account_environment() {
    let dir = tempfile::tempdir().unwrap();
    write_image(&dir.path().join("page.png"));
    let token_path = dir.path().join("google-token.json");
    tahweel_drive::OAuthToken::new("expired", "mock-refresh", Duration::ZERO)
        .unwrap()
        .save(&token_path)
        .unwrap();
    let server = mock_drive().await;
    let mut cmd = isolated_command(dir.path());
    cmd.env("TAHWEEL_SERVICE_ACCOUNTS", "missing-key.json")
        .env("PATH", "")
        .args([
            "page.png",
            "--recognizer",
            "drive-oauth",
            "--drive-test-url",
            &server.uri(),
            "--drive-token",
        ])
        .arg(&token_path)
        .arg("-q");
    let output = run(cmd).await;
    success(&output);
    assert!(!stderr(&output).contains("Sign in to Google:"));
    assert_outputs(dir.path(), "page", 1, "");
    let token: serde_json::Value = serde_json::from_slice(&fs::read(token_path).unwrap()).unwrap();
    assert_eq!(token["access_token"], "mock-access");
    let requests = server.received_requests().await.unwrap();
    let refresh = requests.iter().find(|r| r.url.path() == "/token").unwrap();
    let form = String::from_utf8(refresh.body.clone()).unwrap();
    assert!(form.contains("grant_type=refresh_token"));
    assert!(form.contains("refresh_token=mock-refresh"));
    assert_round_trips(&server, 1).await;
}

#[tokio::test]
async fn setup_errors_return_one_and_input_errors_return_three() {
    let dir = tempfile::tempdir().unwrap();
    let output = isolated_command(dir.path())
        .args([
            "page.png",
            "--recognizer",
            "drive-service-account",
            "--service-account",
            "missing.json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    write_image(&dir.path().join("page.png"));
    write_key(&dir.path().join("key.json"));
    let server = mock_drive().await;
    let mut cmd = service_command(dir.path(), &server);
    cmd.args(["missing.png", "page.png"]);
    let output = run(cmd).await;
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert_outputs(dir.path(), "page", 1, "");
    assert_round_trips(&server, 1).await;
}

#[tokio::test]
async fn missing_pdfium_is_a_run_error_before_any_upload() {
    let dir = tempfile::tempdir().unwrap();
    fixtures::pdf(&dir.path().join("book.pdf"), &[20]);
    write_key(&dir.path().join("key.json"));
    let server = mock_drive().await;
    let mut cmd = service_command(dir.path(), &server);
    cmd.args(["book.pdf", "--pdfium", "missing-library"]);
    let output = run(cmd).await;
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("PDFium"));
    assert!(stderr(&output).contains("--pdfium"));
    assert_round_trips(&server, 0).await;
}

#[tokio::test]
async fn checkpoints_resume_completed_pages_and_separate_languages() {
    let Some(pdfium) = fixtures::pdfium_library() else {
        return;
    };
    for language in ["ar", "en"] {
        let dir = tempfile::tempdir().unwrap();
        let cache = if cfg!(windows) {
            dir.path().join("AppData").join("Local")
        } else if cfg!(target_os = "macos") {
            dir.path().join("Library/Caches")
        } else {
            dir.path().join("cache")
        };
        let checkpoints = cache.join("tahweel/checkpoints");
        fixtures::pdf(&dir.path().join("book.pdf"), &[20, 30]);
        write_key(&dir.path().join("key.json"));
        let server = mock_drive().await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/page-1/export"))
            .respond_with(ResponseTemplate::new(400))
            .with_priority(1)
            .mount(&server)
            .await;
        let invoke = |language: &str| {
            let mut cmd = service_command(dir.path(), &server);
            cmd.args([
                "book.pdf",
                "--drive-concurrency",
                "1",
                "--dpi",
                "72",
                "--language",
                language,
                "--pdfium",
            ])
            .arg(&pdfium);
            cmd
        };
        let failed = run(invoke("ar")).await;
        assert_eq!(failed.status.code(), Some(3), "{}", stderr(&failed));
        assert!(!dir.path().join("book.txt").exists());
        assert_eq!(fs::read_dir(&checkpoints).unwrap().count(), 1);
        success(&run(invoke(language)).await);
        assert_outputs(dir.path(), "book", 2, "\n\nPAGE_SEPARATOR\n\n");
        assert_round_trips(&server, if language == "ar" { 3 } else { 4 }).await;
        if language == "en" {
            success(&run(invoke("ar")).await);
        }
        assert_eq!(fs::read_dir(&checkpoints).unwrap().count(), 0);
    }
}

#[cfg(unix)]
async fn wait_for_export(server: &MockServer) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|request| request.url.path().ends_with("/export"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("CLI must reach the mock export endpoint");
}

#[cfg(unix)]
fn interrupt(child: &tokio::process::Child) {
    assert!(
        Command::new("/bin/kill")
            .args(["-INT", &child.id().unwrap().to_string()])
            .status()
            .unwrap()
            .success()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn first_ctrl_c_finishes_cleanup_and_second_ctrl_c_exits_immediately() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};

    for (hard, export_status) in [(false, 200), (false, 401), (true, 200)] {
        let dir = tempfile::tempdir().unwrap();
        for name in ["first.png", "second.png"] {
            write_image(&dir.path().join(name));
        }
        write_key(&dir.path().join("key.json"));
        let server = mock_drive().await;
        Mock::given(method("GET"))
            .and(path("/drive/v3/files/page-0/export"))
            .respond_with(
                ResponseTemplate::new(export_status)
                    .set_body_string(TEXT)
                    .set_delay(Duration::from_secs(if hard { 30 } else { 3 })),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        let mut cmd = service_command(dir.path(), &server);
        cmd.args(["first.png", "second.png", "--drive-concurrency", "1"]);
        let mut child = tokio::process::Command::from(cmd)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        wait_for_export(&server).await;
        interrupt(&child);
        let notice = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(notice.contains("Press Ctrl-C again"), "{notice}");
        if hard {
            interrupt(&child);
        }
        let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output.status.code(), Some(130));
        if !hard {
            assert_round_trips(&server, 1).await;
        }
        assert!(!dir.path().join("second.txt").exists());
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn missing_and_rejected_saved_sign_ins_use_the_loopback_flow() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};

    for rejected in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        write_image(&dir.path().join("page.png"));
        // Linux's default path is exercised here; no --drive-token is passed.
        let token_path = dir.path().join("data/tahweel/google-token.json");
        if rejected {
            tahweel_drive::OAuthToken::new("old-access", "rejected-refresh", Duration::ZERO)
                .unwrap()
                .save(&token_path)
                .unwrap();
        }
        let server = mock_drive().await;
        Mock::given(method("POST")).and(path("/token"))
            .respond_with(|request: &Request| {
                let body = String::from_utf8_lossy(&request.body);
                if body.contains("refresh_token=rejected-refresh") {
                    ResponseTemplate::new(400).set_body_json(json!({"error":"invalid_grant"}))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "access_token":"mock-access", "refresh_token":"mock-refresh", "expires_in":3600, "token_type":"Bearer",
                    }))
                }
            }).with_priority(1).mount(&server).await;
        let mut cmd = isolated_command(dir.path());
        // webbrowser honors BROWSER on Linux. This successful no-op never opens a real browser.
        cmd.env("BROWSER", "/bin/true").env("PATH", "").args([
            "convert",
            "page.png",
            "--recognizer",
            "drive-oauth",
            "--drive-test-url",
            &server.uri(),
            "-q",
        ]);
        let mut child = tokio::process::Command::from(cmd)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let mut messages = Vec::new();
        let url = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let line = lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("sign-in URL on stderr");
                messages.push(line.clone());
                if let Some(url) = line.strip_prefix("Sign in to Google: ") {
                    break reqwest::Url::parse(url).unwrap();
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(url.path(), "/authorize");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        let params = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        let mut callback = reqwest::Url::parse(&params["redirect_uri"]).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("state", &params["state"])
            .append_pair("code", "mock-code");
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(callback)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        success(&output);
        assert_eq!(
            messages
                .iter()
                .any(|line| line.contains("Google rejected the saved sign-in")),
            rejected
        );
        assert!(token_path.exists());
        assert_outputs(dir.path(), "page", 1, "");
        assert_round_trips(&server, 1).await;
    }
}
