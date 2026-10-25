//! Public pipeline behavior with generated PDFs, local images and a deterministic fake recognizer.
mod support;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use support::{pdf, pdfium_library};

use async_trait::async_trait;
use tahweel::{
    BoxError, ColorMode, DocumentStatus, Error, Event, Format, ImageEncoding, ImageFormat, Input,
    NoProgress, OutputMode, OutputWriter, PageImage, PageOptions, Recognition, Recognizer, Tahweel,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

struct Fake {
    concurrency: usize,
    starts: AtomicUsize,
    active: Arc<AtomicUsize>,
    peak: AtomicUsize,
    completed: AtomicUsize,
    started: Semaphore,
    gate: Option<Semaphore>,
    limit: Option<usize>,
    fail_width: Option<u32>,
    fatal_width: Option<u32>,
    seen: std::sync::Mutex<Vec<PageImage>>,
    capture: bool,
    text_override: Option<String>,
}

impl Fake {
    fn new(concurrency: usize) -> Self {
        Self {
            concurrency,
            starts: AtomicUsize::new(0),
            active: Arc::new(AtomicUsize::new(0)),
            peak: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            started: Semaphore::new(0),
            gate: None,
            limit: None,
            fail_width: None,
            fatal_width: None,
            seen: std::sync::Mutex::new(Vec::new()),
            capture: true,
            text_override: None,
        }
    }

    async fn wait_for_starts(&self, count: u32) {
        tokio::time::timeout(Duration::from_secs(10), self.started.acquire_many(count))
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
}

struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Recognizer for Fake {
    async fn recognize(&self, page: &PageImage) -> Result<Recognition, BoxError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let _guard = Active(self.active.clone());
        self.peak.fetch_max(active, Ordering::SeqCst);
        self.started.add_permits(1);
        assert!(self.limit.is_none_or(|limit| page.data.len() <= limit));
        if self.capture {
            self.seen.lock().unwrap().push(page.clone());
        }
        if let Some(gate) = &self.gate {
            gate.acquire().await.unwrap().forget();
        } else {
            // The first page of each group finishes last.
            tokio::time::sleep(Duration::from_millis(u64::from(4 - page.width % 4) * 10)).await;
        }
        if self.fatal_width == Some(page.width) {
            return Err(Box::new(Error::RecognizerUnavailable {
                source: std::io::Error::other("no usable accounts").into(),
            }));
        }
        if self.fail_width == Some(page.width) {
            return Err(std::io::Error::other("page failed").into());
        }
        self.completed.fetch_add(1, Ordering::SeqCst);
        Ok(Recognition::new(
            self.text_override
                .clone()
                .unwrap_or_else(|| format!("page width {}", page.width)),
        ))
    }

    fn max_in_flight(&self) -> usize {
        self.concurrency
    }
    fn max_image_bytes(&self) -> Option<usize> {
        self.limit
    }
}

fn image(path: &Path, width: u32, height: u32) {
    image::RgbImage::from_fn(width, height, |x, y| {
        let value = x
            .wrapping_mul(1_103_515_245)
            .wrapping_add(y.wrapping_mul(12_345));
        image::Rgb([
            value.to_le_bytes()[0],
            value.to_le_bytes()[1],
            value.to_le_bytes()[2],
        ])
    })
    .save(path)
    .unwrap();
}

fn images(root: &Path, count: u32) -> Vec<Input> {
    (0..count)
        .map(|index| {
            let path = root.join(format!("{index:03}.png"));
            image(&path, 20 + index, 10);
            Input::from(path)
        })
        .collect()
}

fn memory_pipeline(fake: Arc<Fake>) -> tahweel::TahweelBuilder {
    Tahweel::builder(fake).output(OutputWriter::new().formats([]))
}

#[tokio::test]
async fn folder_uses_full_concurrency_and_returns_documents_in_input_order() {
    let dir = tempfile::tempdir().unwrap();
    images(dir.path(), 12);
    let fake = Arc::new(Fake::new(3));
    let events = std::sync::Mutex::new(Vec::new());
    let report = memory_pipeline(fake.clone())
        .build()
        .run([Input::from(dir.path())], &|event| {
            events.lock().unwrap().push(event);
        })
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 12);
    assert_eq!(fake.peak.load(Ordering::SeqCst), 3);
    assert_eq!(report.documents.len(), 12);
    for (index, document) in report.documents.iter().enumerate() {
        assert_eq!(document.document.id, format!("{index:03}"));
        let DocumentStatus::Converted { pages, outputs } = &document.status else {
            panic!("not converted")
        };
        assert_eq!(pages, &[format!("page width {}", index + 20)]);
        assert_eq!(outputs, &Vec::<std::path::PathBuf>::new());
    }
    let finished: Vec<_> = events
        .into_inner()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            Event::DocumentFinished { document, .. } => Some(document.id),
            _ => None,
        })
        .collect();
    assert_ne!(
        finished,
        (0..12).map(|i| format!("{i:03}")).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn failed_document_keeps_its_checkpoint_and_other_documents_finish() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = images(dir.path(), 5);
    let out = dir.path().join("output");
    let checkpoints = dir.path().join("checkpoints");
    let fake = Arc::new(Fake {
        fail_width: Some(22),
        ..Fake::new(2)
    });
    let report = Tahweel::builder(fake)
        .output(OutputWriter::new().formats([Format::Txt]))
        .output_mode(OutputMode::Flat(out.clone()))
        .checkpoints(&checkpoints)
        .checkpoint_label("fake:ar")
        .build()
        .run(inputs, &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 1);
    assert!(matches!(
        report.documents[2].status,
        DocumentStatus::Failed(_)
    ));
    assert!(!out.join("002.txt").exists());
    assert_eq!(fs::read_dir(out).unwrap().count(), 4);
    assert_eq!(fs::read_dir(checkpoints).unwrap().count(), 1);
}

#[tokio::test]
async fn graceful_cancel_finishes_in_flight_calls_and_starts_no_more() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = images(dir.path(), 20);
    let fake = Arc::new(Fake {
        gate: Some(Semaphore::new(0)),
        ..Fake::new(3)
    });
    let token = CancellationToken::new();
    let pipeline = memory_pipeline(fake.clone())
        .cancellation_token(token.clone())
        .checkpoints(dir.path().join("cp"))
        .build();
    let run = tokio::spawn(async move { pipeline.run(inputs, &NoProgress).await });
    fake.wait_for_starts(3).await;
    token.cancel();
    fake.gate.as_ref().unwrap().add_permits(3);
    let report = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 3);
    assert_eq!(fake.completed.load(Ordering::SeqCst), 3);
    assert_eq!(
        report
            .documents
            .iter()
            .filter(|doc| matches!(doc.status, DocumentStatus::Converted { .. }))
            .count(),
        3
    );
    assert_eq!(
        report
            .documents
            .iter()
            .filter(|doc| matches!(doc.status, DocumentStatus::Cancelled))
            .count(),
        17
    );
}

#[tokio::test]
async fn dropping_the_run_aborts_recognition_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = images(dir.path(), 20);
    let fake = Arc::new(Fake {
        gate: Some(Semaphore::new(0)),
        ..Fake::new(2)
    });
    let pipeline = memory_pipeline(fake.clone()).build();
    let run = tokio::spawn(async move { pipeline.run(inputs, &NoProgress).await });
    fake.wait_for_starts(2).await;
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(10), async {
        while fake.active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fake.starts.load(Ordering::SeqCst), 2);
    assert_eq!(fake.completed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn output_collisions_are_planned_before_skip_existing() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("Book.png");
    let second = dir.path().join("book.jpg");
    image(&first, 20, 10);
    image(&second, 21, 10);
    let out = dir.path().join("out");
    fs::create_dir(&out).unwrap();
    fs::write(out.join("Book.txt"), b"existing").unwrap();
    let fake = Arc::new(Fake::new(2));
    let pipeline = Tahweel::builder(fake.clone())
        .output(OutputWriter::new().formats([Format::Txt]))
        .output_mode(OutputMode::Flat(out.clone()))
        .skip_existing(true)
        .build();
    let report = pipeline
        .run([Input::from(&first), Input::from(&second)], &NoProgress)
        .await
        .unwrap();
    assert!(matches!(
        report.documents[0].status,
        DocumentStatus::Skipped
    ));
    assert_eq!(report.documents[1].document.id, "book-2");
    assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read(out.join("Book.txt")).unwrap(), b"existing");
    assert!(out.join("book-2.txt").is_file());
    let again = pipeline
        .run([Input::from(first), Input::from(second)], &NoProgress)
        .await
        .unwrap();
    assert!(
        again
            .documents
            .iter()
            .all(|doc| matches!(doc.status, DocumentStatus::Skipped))
    );
    assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn skipped_pdfs_need_no_pdf_runtime_and_bad_inputs_do_not_stop_images() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("old.pdf");
    fs::write(&pdf, b"%PDF-1.7").unwrap();
    fs::write(dir.path().join("old.txt"), b"already done").unwrap();
    let png = dir.path().join("image.png");
    image(&png, 20, 10);
    let fake = Arc::new(Fake::new(2));
    let report = Tahweel::builder(fake.clone())
        .output(OutputWriter::new().formats([Format::Txt]))
        .skip_existing(true)
        .pdfium_library(dir.path().join("missing-runtime"))
        .build()
        .run(
            [
                Input::from(pdf),
                Input::from(dir.path().join("missing")),
                Input::from(png),
            ],
            &NoProgress,
        )
        .await
        .unwrap();
    assert_eq!(report.failures(), 1);
    assert_eq!(report.documents.len(), 2);
    assert!(matches!(
        report.documents[0].status,
        DocumentStatus::Skipped
    ));
    assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fatal_recognizer_errors_stop_the_run_after_other_calls_finish() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = images(dir.path(), 20);
    let fake = Arc::new(Fake {
        fatal_width: Some(20),
        ..Fake::new(2)
    });
    let error = memory_pipeline(fake.clone())
        .build()
        .run(inputs, &NoProgress)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Recognition { .. }));
    assert!(fake.starts.load(Ordering::SeqCst) < 20);
    assert_eq!(fake.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn encoding_honors_color_long_edge_and_recognizer_byte_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noise.png");
    image(&path, 600, 300);
    let fake = Arc::new(Fake {
        limit: Some(4_000),
        ..Fake::new(1)
    });
    let mut options = PageOptions::default();
    options.encoding = ImageEncoding::Png;
    options.color = ColorMode::Grayscale;
    options.max_long_edge = Some(500);
    let report = memory_pipeline(fake.clone())
        .page_options(options)
        .build()
        .run([Input::from(path)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    let pages = fake.seen.lock().unwrap();
    assert_eq!(pages.len(), 1);
    let page = &pages[0];
    assert!(page.data.len() <= 4_000);
    assert!(page.width < 500);
    assert!((i64::from(page.width) - 2 * i64::from(page.height)).abs() <= 2);
    let decoded = image::load_from_memory(&page.data).unwrap();
    assert_eq!(decoded.color(), image::ColorType::L8);
}

#[tokio::test]
async fn impossible_image_limit_fails_before_recognition() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = images(dir.path(), 1);
    let fake = Arc::new(Fake {
        limit: Some(1),
        ..Fake::new(1)
    });
    let report = memory_pipeline(fake.clone())
        .build()
        .run(inputs, &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 1);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 0);
    assert!(
        matches!(&report.documents[0].status, DocumentStatus::Failed(error) if matches!(error.as_ref(), Error::PagePreparation { source, .. } if matches!(source.as_ref(), Error::ImageTooLarge { .. })))
    );
}

#[tokio::test]
async fn transparent_images_are_composited_on_white_and_color_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alpha.png");
    let mut pixels = image::RgbaImage::from_pixel(2, 1, image::Rgba([0, 0, 0, 0]));
    pixels.put_pixel(1, 0, image::Rgba([255, 0, 0, 255]));
    pixels.save(&path).unwrap();
    let fake = Arc::new(Fake::new(1));
    let mut options = PageOptions::default();
    options.color = ColorMode::Color;
    options.encoding = ImageEncoding::Png;
    let report = memory_pipeline(fake.clone())
        .page_options(options)
        .build()
        .run([Input::from(path)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    let seen = fake.seen.lock().unwrap();
    let decoded = image::load_from_memory(&seen[0].data).unwrap().into_rgb8();
    assert_eq!(*decoded.get_pixel(0, 0), image::Rgb([255; 3]));
    assert_eq!(*decoded.get_pixel(1, 0), image::Rgb([255, 0, 0]));
}

#[tokio::test]
async fn invalid_settings_fail_before_inputs_or_recognition() {
    let fake = Arc::new(Fake::new(0));
    assert!(
        memory_pipeline(fake)
            .build()
            .run([], &NoProgress)
            .await
            .is_err()
    );
    for options in [
        {
            let mut p = PageOptions::default();
            p.dpi = 0;
            p
        },
        {
            let mut p = PageOptions::default();
            p.encoding = ImageEncoding::Jpeg { quality: 0 };
            p
        },
        {
            let mut p = PageOptions::default();
            p.max_long_edge = Some(0);
            p
        },
    ] {
        assert!(
            memory_pipeline(Arc::new(Fake::new(1)))
                .page_options(options)
                .build()
                .run([], &NoProgress)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn image_headers_and_exif_orientation_are_honored() {
    use image::ImageEncoder;

    let dir = tempfile::tempdir().unwrap();
    // A little-endian TIFF IFD with Orientation = 6 (rotate 90 degrees clockwise).
    let exif = vec![
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
    ];
    let pixels = image::RgbImage::from_pixel(30, 20, image::Rgb([100, 150, 200]));
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new(&mut bytes);
    encoder.set_exif_metadata(exif).unwrap();
    encoder.encode_image(&pixels).unwrap();
    let path = dir.path().join("page.data");
    fs::write(&path, bytes).unwrap();
    let fake = Arc::new(Fake::new(1));
    let report = memory_pipeline(fake.clone())
        .build()
        .run([Input::from(path)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    let seen = fake.seen.lock().unwrap();
    assert_eq!((seen[0].width, seen[0].height), (20, 30));
}

#[tokio::test]
async fn complete_checkpoints_retry_outputs_without_recognizing_again() {
    let dir = tempfile::tempdir().unwrap();
    let inputs = images(dir.path(), 1);
    let cp = dir.path().join("cp");
    let out = dir.path().join("out");
    let fake = Arc::new(Fake {
        text_override: Some("text\0with a control".into()),
        ..Fake::new(1)
    });
    let first = Tahweel::builder(fake.clone())
        .checkpoints(&cp)
        .output_mode(OutputMode::Flat(out.clone()))
        .build()
        .run(inputs.clone(), &NoProgress)
        .await
        .unwrap();
    assert_eq!(first.failures(), 1);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&cp).unwrap().count(), 1);
    let resumed = AtomicUsize::new(0);
    let second = Tahweel::builder(fake.clone())
        .checkpoints(&cp)
        .output_mode(OutputMode::Flat(out.clone()))
        .output(OutputWriter::new().formats([Format::Txt]))
        .build()
        .run(inputs, &|event| {
            if let Event::DocumentStarted { resumed: count, .. } = event {
                resumed.store(count, Ordering::SeqCst);
            }
        })
        .await
        .unwrap();
    assert_eq!(second.failures(), 0);
    assert_eq!(resumed.load(Ordering::SeqCst), 1);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    assert_eq!(
        fs::read(out.join("000.txt")).unwrap(),
        b"text\0with a control"
    );
    assert_eq!(fs::read_dir(cp).unwrap().count(), 0);
}

#[tokio::test]
async fn png_and_jpeg_try_the_quality_ladder_before_shrinking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noise.png");
    let mut pixels = noise_pixels().into_luma8();
    for pixel in pixels.pixels_mut() {
        pixel.0[0] = 64 + pixel.0[0] / 2;
    }
    let pixels = image::DynamicImage::from(pixels);
    pixels.save(&path).unwrap();
    for (encoding, qualities) in [
        (ImageEncoding::Png, vec![95, 85, 75, 60]),
        (ImageEncoding::Jpeg { quality: 90 }, vec![85, 75, 60]),
        (ImageEncoding::Jpeg { quality: 85 }, vec![75, 60]),
        (ImageEncoding::Jpeg { quality: 75 }, vec![60]),
    ] {
        for quality in qualities {
            let expected = jpeg_bytes(&pixels, quality);
            let fake = Arc::new(Fake {
                limit: Some(expected.len()),
                ..Fake::new(1)
            });
            let mut options = PageOptions::default();
            options.encoding = encoding;
            let report = memory_pipeline(fake.clone())
                .page_options(options)
                .build()
                .run([Input::from(path.clone())], &NoProgress)
                .await
                .unwrap();
            assert_eq!(report.failures(), 0, "{encoding:?} to JPEG {quality}");
            let pages = fake.seen.lock().unwrap();
            assert_eq!((pages[0].width, pages[0].height), (256, 128));
            assert_eq!(pages[0].format, ImageFormat::Jpeg);
            assert_eq!(pages[0].data.as_ref(), expected);
        }
    }
}

fn noise_pixels() -> image::DynamicImage {
    let mut state = 42_u32;
    image::GrayImage::from_fn(256, 128, |_, _| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        image::Luma([state.to_le_bytes()[0]])
    })
    .into()
}

fn jpeg_bytes(pixels: &image::DynamicImage, quality: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
        .encode(
            pixels.as_bytes(),
            pixels.width(),
            pixels.height(),
            pixels.color().into(),
        )
        .unwrap();
    bytes
}

#[tokio::test]
async fn oversized_pages_shrink_proportionally_from_source_pixels_as_jpeg_75() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("noise.png");
    let pixels = noise_pixels();
    pixels.save(&path).unwrap();
    let smallest = jpeg_bytes(&pixels, 60).len();
    for (encoding, limit, expected_dimensions) in [
        (ImageEncoding::Png, smallest - 1, Some((230, 115))),
        (ImageEncoding::Png, smallest / 4, Some((121, 60))),
        // Starting below the ladder must shrink, not increase quality at full size.
        // JPEG 75 still exceeds this limit after the first 0.90 resize.
        (
            ImageEncoding::Jpeg { quality: 1 },
            jpeg_bytes(&pixels, 1).len() - 1,
            None,
        ),
    ] {
        let fake = Arc::new(Fake {
            limit: Some(limit),
            ..Fake::new(1)
        });
        let mut options = PageOptions::default();
        options.encoding = encoding;
        let report = memory_pipeline(fake.clone())
            .page_options(options)
            .build()
            .run([Input::from(path.clone())], &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.failures(), 0);
        let pages = fake.seen.lock().unwrap();
        let page = &pages[0];
        assert!(page.data.len() <= limit);
        assert_eq!(page.format, ImageFormat::Jpeg);
        if let Some(dimensions) = expected_dimensions {
            assert_eq!((page.width, page.height), dimensions);
        } else {
            assert!(page.width < 230 && page.height < 115);
        }
        assert!((i64::from(page.width) - 2 * i64::from(page.height)).abs() <= 1);
        let expected = pixels.resize_exact(
            page.width,
            page.height,
            image::imageops::FilterType::Triangle,
        );
        assert_eq!(page.data.as_ref(), jpeg_bytes(&expected, 75));
    }
}

#[tokio::test]
async fn default_image_settings_cap_the_long_edge_without_upscaling() {
    let defaults = PageOptions::default();
    assert_eq!(defaults.dpi, 200);
    assert_eq!(defaults.color, ColorMode::Grayscale);
    assert_eq!(defaults.encoding, ImageEncoding::Png);
    assert_eq!(defaults.max_long_edge, Some(2_339));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("page.png");
    for (dimensions, expected, cap) in [
        ((4_678, 2_338), (2_339, 1_169), defaults.max_long_edge),
        ((2_338, 4_678), (1_169, 2_339), defaults.max_long_edge),
        ((2_339, 500), (2_339, 500), defaults.max_long_edge),
        ((320, 160), (320, 160), defaults.max_long_edge),
        ((2_400, 100), (2_400, 100), None),
    ] {
        image::RgbImage::from_pixel(dimensions.0, dimensions.1, image::Rgb([80, 100, 120]))
            .save(&path)
            .unwrap();
        let fake = Arc::new(Fake::new(1));
        let mut options = defaults.clone();
        options.max_long_edge = cap;
        let report = memory_pipeline(fake.clone())
            .page_options(options)
            .build()
            .run([Input::from(path.clone())], &NoProgress)
            .await
            .unwrap();
        assert_eq!(report.failures(), 0);
        let pages = fake.seen.lock().unwrap();
        assert_eq!((pages[0].width, pages[0].height), expected);
        assert_eq!(pages[0].format, ImageFormat::Png);
        assert_eq!(
            image::load_from_memory(&pages[0].data).unwrap().color(),
            image::ColorType::L8
        );
    }
}

#[tokio::test]
async fn repeated_inputs_keep_a_shared_checkpoint_until_every_copy_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let input = images(dir.path(), 1).remove(0);
    let cp = dir.path().join("cp");
    let token = CancellationToken::new();
    let fake = Arc::new(Fake::new(1));
    let first = memory_pipeline(fake.clone())
        .checkpoints(&cp)
        .cancellation_token(token.clone())
        .build()
        .run([input.clone(), input.clone()], &|event| {
            if matches!(event, Event::DocumentFinished { .. }) {
                token.cancel();
            }
        })
        .await
        .unwrap();
    assert!(first.cancelled);
    assert!(matches!(
        first.documents[0].status,
        DocumentStatus::Converted { .. }
    ));
    assert!(matches!(
        first.documents[1].status,
        DocumentStatus::Cancelled
    ));
    assert_eq!(first.documents[0].document.id, "000");
    assert_eq!(first.documents[1].document.id, "000-2");
    assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read_dir(&cp).unwrap().count(), 1);
    let resumed = Arc::new(Fake::new(2));
    let second = memory_pipeline(resumed.clone())
        .checkpoints(&cp)
        .build()
        .run([input.clone(), input], &NoProgress)
        .await
        .unwrap();
    assert_eq!(second.failures(), 0);
    assert!(
        second
            .documents
            .iter()
            .all(|document| matches!(document.status, DocumentStatus::Converted { .. }))
    );
    assert_eq!(resumed.starts.load(Ordering::SeqCst), 0);
    assert_eq!(fs::read_dir(cp).unwrap().count(), 0);
}

#[tokio::test]
async fn a_300_page_pdf_is_recognized_in_page_order_with_bounded_concurrency() {
    let Some(library) = pdfium_library() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.pdf");
    pdf(&path, &(20..320).collect::<Vec<_>>());
    let fake = Arc::new(Fake {
        capture: false,
        ..Fake::new(4)
    });
    let mut options = PageOptions::default();
    options.dpi = 72;
    let report = memory_pipeline(fake.clone())
        .pdfium_library(&library)
        .page_options(options)
        .build()
        .run([Input::from(path)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    assert_eq!(fake.peak.load(Ordering::SeqCst), 4);
    let DocumentStatus::Converted { pages, .. } = &report.documents[0].status else {
        panic!("PDF failed")
    };
    assert_eq!(
        pages,
        &(20..320)
            .map(|width| format!("page width {width}"))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_failed_pdf_retains_completed_pages_and_resume_recognizes_only_missing_pages() {
    let Some(library) = pdfium_library() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.pdf");
    pdf(&path, &(20..26).collect::<Vec<_>>());
    let cp = dir.path().join("checkpoints");
    let out = dir.path().join("out");
    let mut options = PageOptions::default();
    options.dpi = 72;
    let failing = Arc::new(Fake {
        fail_width: Some(22),
        ..Fake::new(1)
    });
    let first = Tahweel::builder(failing.clone())
        .pdfium_library(&library)
        .page_options(options.clone())
        .output(OutputWriter::new().formats([Format::Txt]))
        .output_mode(OutputMode::Flat(out.clone()))
        .checkpoints(&cp)
        .checkpoint_label("fake:ar")
        .build()
        .run([Input::from(&path)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(first.failures(), 1);
    assert_eq!(failing.starts.load(Ordering::SeqCst), 3);
    assert!(!out.join("book.txt").exists());
    let checkpoint = fs::read_dir(&cp).unwrap().next().unwrap().unwrap().path();
    let text = fs::read_to_string(&checkpoint).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(text.contains("\"page\":1"));
    let successful = Arc::new(Fake::new(2));
    let resumed = AtomicUsize::new(0);
    let report = Tahweel::builder(successful.clone())
        .pdfium_library(&library)
        .page_options(options)
        .output(OutputWriter::new().formats([Format::Txt]))
        .output_mode(OutputMode::Flat(out.clone()))
        .checkpoints(&cp)
        .checkpoint_label("fake:ar")
        .build()
        .run([Input::from(&path)], &|event| {
            if let Event::DocumentStarted { resumed: count, .. } = event {
                resumed.store(count, Ordering::SeqCst);
            }
        })
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    assert_eq!(resumed.load(Ordering::SeqCst), 2);
    assert_eq!(successful.starts.load(Ordering::SeqCst), 4);
    assert_eq!(fs::read_dir(cp).unwrap().count(), 0);
    assert!(out.join("book.txt").is_file());
}

#[tokio::test]
async fn corrupt_checkpoints_are_replaced_and_configuration_changes_do_not_resume() {
    let Some(library) = pdfium_library() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.pdf");
    pdf(&path, &[20, 21, 22]);
    let cp = dir.path().join("checkpoints");
    let mut options = PageOptions::default();
    options.dpi = 72;
    let failing = Arc::new(Fake {
        fail_width: Some(21),
        ..Fake::new(1)
    });
    let pipeline = memory_pipeline(failing)
        .pdfium_library(&library)
        .page_options(options.clone())
        .checkpoints(&cp)
        .checkpoint_label("fake:ar")
        .build();
    pipeline
        .run([Input::from(&path)], &NoProgress)
        .await
        .unwrap();
    let checkpoint = fs::read_dir(&cp).unwrap().next().unwrap().unwrap().path();
    fs::write(
        &checkpoint,
        "{\"page\":1,\"text\":\"wrong cached value\"}\n{incomplete",
    )
    .unwrap();
    let successful = Arc::new(Fake::new(2));
    let report = memory_pipeline(successful.clone())
        .pdfium_library(&library)
        .page_options(options.clone())
        .checkpoints(&cp)
        .checkpoint_label("fake:ar")
        .build()
        .run([Input::from(&path)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    assert_eq!(successful.starts.load(Ordering::SeqCst), 3);
    pipeline
        .run([Input::from(&path)], &NoProgress)
        .await
        .unwrap();
    let different = Arc::new(Fake::new(2));
    let report = memory_pipeline(different.clone())
        .pdfium_library(&library)
        .page_options(options)
        .checkpoints(&cp)
        .checkpoint_label("fake:en")
        .build()
        .run([Input::from(&path)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    assert_eq!(different.starts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn cancelling_a_pdf_saves_active_pages_for_resume() {
    let Some(library) = pdfium_library() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.pdf");
    pdf(&path, &(20..50).collect::<Vec<_>>());
    let cp = dir.path().join("cp");
    let fake = Arc::new(Fake {
        gate: Some(Semaphore::new(0)),
        ..Fake::new(3)
    });
    let token = CancellationToken::new();
    let mut options = PageOptions::default();
    options.dpi = 72;
    let pipeline = memory_pipeline(fake.clone())
        .pdfium_library(&library)
        .page_options(options.clone())
        .checkpoints(&cp)
        .cancellation_token(token.clone())
        .build();
    let input = Input::from(&path);
    let run = tokio::spawn(async move { pipeline.run([input], &NoProgress).await });
    fake.wait_for_starts(3).await;
    token.cancel();
    fake.gate.as_ref().unwrap().add_permits(3);
    let report = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(report.cancelled);
    assert!(matches!(
        report.documents[0].status,
        DocumentStatus::Cancelled
    ));
    assert_eq!(fake.starts.load(Ordering::SeqCst), 3);
    assert_eq!(fake.completed.load(Ordering::SeqCst), 3);
    let checkpoint = fs::read_dir(&cp).unwrap().next().unwrap().unwrap().path();
    assert_eq!(fs::read_to_string(checkpoint).unwrap().lines().count(), 3);
    let next = Arc::new(Fake::new(3));
    let resumed = memory_pipeline(next.clone())
        .pdfium_library(&library)
        .page_options(options)
        .checkpoints(&cp)
        .build()
        .run([Input::from(path)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(resumed.failures(), 0);
    assert_eq!(next.starts.load(Ordering::SeqCst), 27);
    let DocumentStatus::Converted { pages, .. } = &resumed.documents[0].status else {
        panic!("not converted")
    };
    assert_eq!(pages.len(), 30);
    assert_eq!(fs::read_dir(cp).unwrap().count(), 0);
}

#[tokio::test]
async fn a_malformed_pdf_does_not_stop_later_documents() {
    let Some(library) = pdfium_library() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.pdf");
    fs::write(&bad, b"%PDF-1.7\nno objects").unwrap();
    let good = dir.path().join("good.pdf");
    pdf(&good, &[40]);
    let image_path = dir.path().join("image.png");
    image(&image_path, 50, 20);
    let fake = Arc::new(Fake::new(2));
    let mut options = PageOptions::default();
    options.dpi = 72;
    let report = memory_pipeline(fake.clone())
        .pdfium_library(&library)
        .page_options(options)
        .build()
        .run(
            [Input::from(bad), Input::from(good), Input::from(image_path)],
            &NoProgress,
        )
        .await
        .unwrap();
    assert_eq!(report.failures(), 1);
    assert!(matches!(
        report.documents[0].status,
        DocumentStatus::Failed(_)
    ));
    assert!(matches!(
        report.documents[1].status,
        DocumentStatus::Converted { .. }
    ));
    assert!(matches!(
        report.documents[2].status,
        DocumentStatus::Converted { .. }
    ));
    assert_eq!(fake.starts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn pdf_text_layers_are_rendered_and_sent_to_the_recognizer() {
    let Some(library) = pdfium_library() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("text.pdf");
    pdf(&path, &[80]);
    let fake = Arc::new(Fake::new(1));
    let mut options = PageOptions::default();
    options.dpi = 144;
    options.encoding = ImageEncoding::Png;
    let report = memory_pipeline(fake.clone())
        .pdfium_library(&library)
        .page_options(options)
        .build()
        .run([Input::from(path)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 0);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 1);
    let pages = fake.seen.lock().unwrap();
    assert_eq!((pages[0].width, pages[0].height), (160, 40));
    let pixels = image::load_from_memory(&pages[0].data)
        .unwrap()
        .into_luma8();
    assert!(pixels.pixels().any(|p| p.0[0] < 128));
    assert!(pixels.pixels().any(|p| p.0[0] == 255));
    let DocumentStatus::Converted { pages, .. } = &report.documents[0].status else {
        panic!("not converted")
    };
    assert_eq!(pages, &["page width 160"]);
}

#[tokio::test]
async fn unsupported_pdf_dimensions_fail_before_bitmap_allocation_or_recognition() {
    let Some(library) = pdfium_library() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.pdf");
    pdf(&path, &[72]);
    let fake = Arc::new(Fake::new(1));
    let mut options = PageOptions::default();
    options.dpi = u32::MAX;
    let report = memory_pipeline(fake.clone())
        .pdfium_library(library)
        .page_options(options)
        .build()
        .run([Input::from(path)], &NoProgress)
        .await
        .unwrap();
    assert_eq!(report.failures(), 1);
    assert_eq!(fake.starts.load(Ordering::SeqCst), 0);
    assert!(
        matches!(&report.documents[0].status, DocumentStatus::Failed(error)
        if matches!(error.as_ref(), Error::PagePreparation { source, .. }
            if matches!(source.as_ref(), Error::PageDimensions { .. })))
    );
}
