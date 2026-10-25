use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pdfium_render::prelude::{Pdfium, PdfiumError};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use crate::checkpoint::Checkpoint;
use crate::{Document, DocumentKind, Error, PageImage, PageOptions, Result};

pub(super) struct SourceJob {
    pub(super) index: usize,
    pub(super) document: Document,
    pub(super) failed: Arc<AtomicBool>,
}

pub(super) enum Message {
    Started {
        document: usize,
        pages: usize,
        texts: BTreeMap<usize, String>,
        checkpoint: Option<Checkpoint>,
    },
    Page {
        document: usize,
        page: usize,
        image: PageImage,
        permit: OwnedSemaphorePermit,
    },
    Failed {
        document: usize,
        error: Error,
    },
}

pub(super) struct Producer {
    pub(super) jobs: Vec<SourceJob>,
    pub(super) options: PageOptions,
    pub(super) pdfium_library: Option<PathBuf>,
    pub(super) limit: Option<usize>,
    pub(super) checkpoints: Option<PathBuf>,
    pub(super) checkpoint_access: Arc<std::sync::Mutex<()>>,
    pub(super) label: String,
    pub(super) stop: CancellationToken,
    pub(super) sender: mpsc::Sender<Message>,
    pub(super) slots: Arc<Semaphore>,
}

impl Producer {
    pub(super) fn run(self) -> Result<()> {
        let pdfium = if !self.stop.is_cancelled()
            && self
                .jobs
                .iter()
                .any(|job| job.document.kind == DocumentKind::Pdf)
        {
            Some(crate::page::load_pdfium(self.pdfium_library.as_deref())?)
        } else {
            None
        };
        for job in &self.jobs {
            if self.stopped(job) {
                continue;
            }
            if let Err(error) = self.document(job, pdfium.as_ref()) {
                job.failed.store(true, Ordering::Release);
                if self
                    .sender
                    .blocking_send(Message::Failed {
                        document: job.index,
                        error,
                    })
                    .is_err()
                {
                    break;
                }
            }
            if self.stop.is_cancelled() || self.sender.is_closed() {
                break;
            }
        }
        Ok(())
    }

    fn stopped(&self, job: &SourceJob) -> bool {
        self.stop.is_cancelled() || self.sender.is_closed() || job.failed.load(Ordering::Acquire)
    }

    fn document(&self, job: &SourceJob, pdfium: Option<&Pdfium>) -> Result<()> {
        let pdf = if job.document.kind == DocumentKind::Pdf {
            // run() initializes PDFium before processing any PDF job.
            Some(
                pdfium
                    .expect("PDF jobs have a loaded runtime")
                    .load_pdf_from_file(&job.document.path, None)
                    .map_err(|source| Error::Pdf {
                        context: format!("opening {}", job.document.path.display()),
                        source,
                    })?,
            )
        } else {
            None
        };
        let pages = if let Some(pdf) = &pdf {
            usize::try_from(pdf.pages().len())
                .ok()
                .filter(|pages| *pages > 0)
                .ok_or_else(|| Error::Pdf {
                    context: format!("opening {}", job.document.path.display()),
                    source: PdfiumError::NoPagesInDocument,
                })?
        } else {
            1
        };
        let (checkpoint, texts) = self.load_checkpoint(job, pages)?;
        let resumed: HashSet<_> = texts.keys().copied().collect();
        if self
            .sender
            .blocking_send(Message::Started {
                document: job.index,
                pages,
                texts,
                checkpoint,
            })
            .is_err()
        {
            return Ok(());
        }
        for page in 1..=pages {
            if self.stopped(job) {
                break;
            }
            if resumed.contains(&page) {
                continue;
            }
            // A permit covers both queued and in-flight images, including the image currently
            // being rendered. No more than 2 * concurrency encoded images can exist at once.
            let Ok(permit) =
                tokio::runtime::Handle::current().block_on(self.slots.clone().acquire_owned())
            else {
                break;
            };
            if self.stopped(job) {
                break;
            }
            let prepared = if let Some(pdf) = &pdf {
                crate::page::render_pdf(pdf, page - 1, &self.options, self.limit)
            } else {
                crate::page::read_image(&job.document.path, &self.options, self.limit)
            };
            let image = prepared.map_err(|source| Error::PagePreparation {
                page,
                source: Box::new(source),
            })?;
            if self.stopped(job) {
                break;
            }
            if self
                .sender
                .blocking_send(Message::Page {
                    document: job.index,
                    page,
                    image,
                    permit,
                })
                .is_err()
            {
                break;
            }
        }
        Ok(())
    }

    fn load_checkpoint(
        &self,
        job: &SourceJob,
        pages: usize,
    ) -> Result<(Option<Checkpoint>, BTreeMap<usize, String>)> {
        let Some(directory) = &self.checkpoints else {
            return Ok((None, BTreeMap::new()));
        };
        let (checkpoint, texts) = Checkpoint::open(
            directory,
            &job.document.path,
            &self.options,
            &self.label,
            pages,
            &self.checkpoint_access,
        )?;
        Ok((Some(checkpoint), texts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_300_page_pdf_never_holds_more_than_eight_encoded_images() {
        let Some(library) = crate::test_support::pdfium_library() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.pdf");
        crate::test_support::pdf(&path, &[20; 300]);
        let slots = Arc::new(Semaphore::new(8));
        let (sender, mut receiver) = mpsc::channel(8);
        let source = Producer {
            jobs: vec![SourceJob {
                index: 0,
                document: Document::new(&path, DocumentKind::Pdf),
                failed: Arc::new(AtomicBool::new(false)),
            }],
            options: PageOptions {
                dpi: 72,
                ..PageOptions::default()
            },
            pdfium_library: Some(library),
            limit: None,
            checkpoints: None,
            checkpoint_access: Arc::default(),
            label: String::new(),
            stop: CancellationToken::new(),
            sender,
            slots: slots.clone(),
        };
        let producer = tokio::task::spawn_blocking(move || source.run());
        assert!(matches!(
            receiver.recv().await.unwrap(),
            Message::Started { pages: 300, .. }
        ));
        let mut held = Vec::new();
        for _ in 0..8 {
            let message = receiver.recv().await.unwrap();
            assert!(matches!(message, Message::Page { .. }));
            held.push(message);
        }
        assert_eq!(slots.available_permits(), 0);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), receiver.recv())
                .await
                .is_err()
        );
        drop(held);
        let mut pages = 8;
        while let Some(message) = receiver.recv().await {
            assert!(matches!(message, Message::Page { .. }));
            pages += 1;
        }
        producer.await.unwrap().unwrap();
        assert_eq!(pages, 300);
        assert_eq!(slots.available_permits(), 8);
    }

    #[tokio::test]
    async fn one_stream_holds_at_most_eight_images_including_work_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let mut jobs = Vec::new();
        for index in 0..300 {
            let path = dir.path().join(format!("{index}.png"));
            image::GrayImage::from_pixel(10, 10, image::Luma([255]))
                .save(&path)
                .unwrap();
            jobs.push(SourceJob {
                index,
                document: Document::new(&path, DocumentKind::Image),
                failed: Arc::new(AtomicBool::new(false)),
            });
        }
        let slots = Arc::new(Semaphore::new(8));
        let (sender, mut receiver) = mpsc::channel(8);
        let source = Producer {
            jobs,
            options: PageOptions::default(),
            pdfium_library: None,
            limit: None,
            checkpoints: None,
            checkpoint_access: Arc::default(),
            label: String::new(),
            stop: CancellationToken::new(),
            sender,
            slots: slots.clone(),
        };
        let producer = tokio::task::spawn_blocking(move || source.run());
        let mut held = Vec::new();
        let mut count = 0;
        while held.len() < 8 {
            let message = receiver.recv().await.unwrap();
            if matches!(message, Message::Page { .. }) {
                held.push(message);
                count += 1;
            }
        }
        // The producer may have queued the next document's metadata, but cannot render its
        // image until a permit is released. This tests the real stream, without an OCR service.
        assert!(matches!(
            receiver.recv().await.unwrap(),
            Message::Started { document: 8, .. }
        ));
        assert_eq!(slots.available_permits(), 0);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), receiver.recv())
                .await
                .is_err()
        );
        drop(held);
        while let Some(message) = receiver.recv().await {
            if matches!(message, Message::Page { .. }) {
                count += 1;
            }
        }
        producer.await.unwrap().unwrap();
        assert_eq!(count, 300);
        assert_eq!(slots.available_permits(), 8);
    }
}
