//! Loading failures run in a separate test process from tests that initialize `PDFium`.

mod support;

use async_trait::async_trait;
use tahweel::{
    BoxError, Error, ErrorChain, Format, Input, NoProgress, OutputWriter, PageImage, Recognition,
    Recognizer, Tahweel,
};

struct Unused;

struct Text;

#[async_trait]
impl Recognizer for Text {
    async fn recognize(&self, _: &PageImage) -> Result<Recognition, BoxError> {
        Ok(Recognition::new("rendered"))
    }
}

#[async_trait]
impl Recognizer for Unused {
    async fn recognize(&self, _: &PageImage) -> Result<Recognition, BoxError> {
        panic!("recognition must not start when the PDF runtime cannot load")
    }
}

#[tokio::test]
async fn an_explicit_missing_library_is_a_run_error_before_any_recognition() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.pdf");
    std::fs::write(&path, b"%PDF-1.7\n").unwrap();
    let image = dir.path().join("image.png");
    image::GrayImage::from_pixel(2, 2, image::Luma([255]))
        .save(&image)
        .unwrap();
    let library = dir.path().join("missing-pdfium");
    let error = Tahweel::builder(Unused)
        .output(OutputWriter::new().formats([Format::Txt]))
        .pdfium_library(&library)
        .build()
        .run([Input::from(image), Input::from(path)], &NoProgress)
        .await
        .unwrap_err();
    let Error::PdfiumUnavailable { attempted, .. } = &error else {
        panic!("unexpected error: {error}")
    };
    assert_eq!(attempted, &[library.display().to_string()]);
    let message = ErrorChain(&error).to_string();
    assert!(message.contains("release archive"));
    assert!(message.contains("--pdfium"));
    assert!(message.contains("8111"));
    assert!(!dir.path().join("book.txt").exists());
}

#[tokio::test]
async fn image_only_and_skipped_pdf_inputs_do_not_load_a_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image.png");
    image::GrayImage::from_pixel(2, 2, image::Luma([255]))
        .save(&image)
        .unwrap();
    let pdf = dir.path().join("book.pdf");
    std::fs::write(&pdf, b"%PDF-1.7\n").unwrap();
    std::fs::write(dir.path().join("book.txt"), "already converted").unwrap();
    let report = Tahweel::builder(Text)
        .output(OutputWriter::new().formats([Format::Txt]))
        .pdfium_library(dir.path().join("missing-runtime"))
        .skip_existing(true)
        .build()
        .run([Input::from(image), Input::from(pdf)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    assert!(matches!(
        report.documents[1].status,
        tahweel::DocumentStatus::Skipped
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("image.txt")).unwrap(),
        "rendered"
    );
}

#[tokio::test]
async fn finds_the_library_beside_a_relocated_executable_from_another_directory() {
    const CHILD: &str = "TAHWEEL_TEST_RELOCATED_PDFIUM";
    if std::env::var_os(CHILD).is_some() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.pdf");
        support::pdf(&path, &[30]);
        let report = Tahweel::builder(Text)
            .output(OutputWriter::new().formats([]))
            .build()
            .run([Input::from(path)], &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.failures(), 0);
        return;
    }
    let Some(library) = support::pdfium_library() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let original = std::env::current_exe().unwrap();
    let executable = dir.path().join(original.file_name().unwrap());
    std::fs::copy(original, &executable).unwrap();
    let destination =
        pdfium_render::prelude::Pdfium::pdfium_platform_library_name_at_path(dir.path());
    std::fs::copy(library, destination).unwrap();
    let working = tempfile::tempdir().unwrap();
    let result = std::process::Command::new(executable)
        .args([
            "--exact",
            "finds_the_library_beside_a_relocated_executable_from_another_directory",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .current_dir(working.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
