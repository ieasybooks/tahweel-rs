mod producer;
mod state;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::{
    Document, Error, Event, Input, OutputMode, OutputWriter, PageOptions, Progress, Recognizer,
    Result,
};
use producer::{Message, SourceJob};
use state::{DocumentState, FinishedPage};

/// Converts local documents with one bounded stream of page images across the whole run.
///
/// Dropping [`run`](Self::run) aborts recognition tasks. For graceful cancellation, set a
/// token with [`TahweelBuilder::cancellation_token`]: running calls finish, new calls stop,
/// and unfinished documents are reported as [`DocumentStatus::Cancelled`].
pub struct Tahweel {
    recognizer: Arc<dyn Recognizer>,
    output: OutputWriter,
    output_mode: OutputMode,
    page_options: PageOptions,
    pdfium_library: Option<PathBuf>,
    recursive: bool,
    skip_existing: bool,
    checkpoints: Option<PathBuf>,
    checkpoint_label: String,
    cancellation: CancellationToken,
}

impl std::fmt::Debug for Tahweel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tahweel")
            .field("output", &self.output)
            .field("output_mode", &self.output_mode)
            .field("page_options", &self.page_options)
            .field("pdfium_library", &self.pdfium_library)
            .field("recursive", &self.recursive)
            .field("skip_existing", &self.skip_existing)
            .field("checkpoints", &self.checkpoints)
            .field("checkpoint_label", &self.checkpoint_label)
            .finish_non_exhaustive()
    }
}

/// Configures a [`Tahweel`] pipeline.
#[derive(Debug)]
pub struct TahweelBuilder {
    pipeline: Tahweel,
}

impl TahweelBuilder {
    /// Selects output formats and the TXT separator. Defaults to all formats.
    #[must_use]
    pub fn output(mut self, writer: OutputWriter) -> Self {
        self.pipeline.output = writer;
        self
    }

    /// Selects output destinations. Defaults to [`OutputMode::SideBySide`].
    #[must_use]
    pub fn output_mode(mut self, mode: OutputMode) -> Self {
        self.pipeline.output_mode = mode;
        self
    }

    /// Selects page rendering and encoding settings.
    #[must_use]
    pub fn page_options(mut self, options: PageOptions) -> Self {
        self.pipeline.page_options = options;
        self
    }

    /// Uses this `PDFium` library file for PDF inputs. Image-only runs never load it.
    ///
    /// Without an explicit path, searches beside the executable, then the system library
    /// search path. No environment variables are read. The upstream renderer initializes
    /// its bindings once per process; subsequent runs reuse the first loaded library.
    #[must_use]
    pub fn pdfium_library(mut self, path: impl Into<PathBuf>) -> Self {
        self.pipeline.pdfium_library = Some(path.into());
        self
    }

    /// Includes nested directories. Directory symlinks found during traversal and nested output directories are skipped.
    #[must_use]
    pub fn recursive(mut self, recursive: bool) -> Self {
        self.pipeline.recursive = recursive;
        self
    }

    /// Skips documents only when every requested output already exists.
    #[must_use]
    pub fn skip_existing(mut self, skip: bool) -> Self {
        self.pipeline.skip_existing = skip;
        self
    }

    /// Saves each recognized page here and resumes matching checkpoints on later runs.
    /// Set [`checkpoint_label`](Self::checkpoint_label) to identify the recognizer and language.
    #[must_use]
    pub fn checkpoints(mut self, directory: impl Into<PathBuf>) -> Self {
        self.pipeline.checkpoints = Some(directory.into());
        self
    }

    /// Identifies the recognizer configuration in checkpoint keys, for example `drive-oauth:ar`.
    /// Change this when recognition settings change. Defaults to an empty label.
    #[must_use]
    pub fn checkpoint_label(mut self, label: impl Into<String>) -> Self {
        self.pipeline.checkpoint_label = label.into();
        self
    }

    /// Stops starting pages when cancelled, while allowing running recognitions to finish.
    #[must_use]
    pub fn cancellation_token(mut self, token: CancellationToken) -> Self {
        self.pipeline.cancellation = token;
        self
    }

    /// Finishes configuration. Page settings and recognizer limits are checked by `run`.
    pub fn build(self) -> Tahweel {
        self.pipeline
    }
}

/// Results in input order, even when documents finish in a different order.
#[derive(Debug, Clone, Default)]
pub struct RunReport {
    /// Every discovered document and its outcome.
    pub documents: Vec<DocumentReport>,
    /// Inputs that could not be expanded.
    pub failed_inputs: Vec<FailedInput>,
    /// Whether graceful cancellation interrupted the run.
    pub cancelled: bool,
}

impl RunReport {
    /// Failed inputs plus failed documents. Cancelled documents are counted separately.
    pub fn failures(&self) -> usize {
        self.failed_inputs.len()
            + self
                .documents
                .iter()
                .filter(|report| matches!(report.status, DocumentStatus::Failed(_)))
                .count()
    }
}

/// An input that could not be expanded into documents.
#[derive(Debug, Clone)]
pub struct FailedInput {
    /// The input as supplied.
    pub input: Input,
    /// Why discovery failed.
    pub error: Arc<Error>,
}

/// Outcome for one document.
#[derive(Debug, Clone)]
pub struct DocumentReport {
    /// The document, with its planned output stem.
    pub document: Document,
    /// Its result.
    pub status: DocumentStatus,
}

/// Outcome for a discovered document.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DocumentStatus {
    /// All pages were recognized and every requested output was written.
    Converted {
        /// Recognized text in page order, also available when no formats were requested.
        pages: Vec<String>,
        /// Written files, in format order.
        outputs: Vec<PathBuf>,
    },
    /// All requested outputs already existed.
    Skipped,
    /// A page, checkpoint or output failed. Partial outputs are never written for failed OCR.
    Failed(Arc<Error>),
    /// Recognition stopped before this document was complete. Saved pages can be resumed.
    Cancelled,
}

impl Tahweel {
    /// Configures a pipeline using a recognizer or an `Arc<dyn Recognizer>`.
    pub fn builder(recognizer: impl Recognizer + 'static) -> TahweelBuilder {
        TahweelBuilder {
            pipeline: Self {
                recognizer: Arc::new(recognizer),
                output: OutputWriter::new(),
                output_mode: OutputMode::default(),
                page_options: PageOptions::default(),
                pdfium_library: None,
                recursive: false,
                skip_existing: false,
                checkpoints: None,
                checkpoint_label: String::new(),
                cancellation: CancellationToken::new(),
            },
        }
    }

    /// Plans every output before OCR, then converts the inputs. Requires a Tokio runtime.
    ///
    /// Ordinary input and document failures are stored in the report. Errors affecting the
    /// whole run, such as an unavailable PDF library or [`Error::RecognizerUnavailable`],
    /// are returned after running recognitions finish and save their checkpoints.
    pub async fn run(
        &self,
        inputs: impl IntoIterator<Item = Input>,
        progress: &dyn Progress,
    ) -> Result<RunReport> {
        self.page_options.validate()?;
        let concurrency = self.recognizer.max_in_flight();
        if concurrency == 0 || concurrency > Semaphore::MAX_PERMITS / 2 {
            return Err(Error::InvalidOptions(
                "recognizer concurrency is out of range".into(),
            ));
        }
        let stop = self.cancellation.child_token();
        let _abort_producer = stop.clone().drop_guard();
        let (mut states, failed_inputs) = self.plan(inputs.into_iter().collect()).await?;
        for failed in &failed_inputs {
            progress.report(Event::InputFailed {
                input: failed.input.clone(),
                error: failed.error.clone(),
            });
        }
        for state in &states {
            state.report_planned(progress);
        }
        let jobs = states
            .iter()
            .enumerate()
            .filter(|(_, state)| state.status.is_none())
            .map(|(index, state)| SourceJob {
                index,
                document: state.document.clone(),
                failed: state.failed.clone(),
            })
            .collect();
        let (sender, receiver) = mpsc::channel(concurrency * 2);
        let source = producer::Producer {
            jobs,
            options: self.page_options.clone(),
            pdfium_library: self.pdfium_library.clone(),
            limit: self.recognizer.max_image_bytes(),
            checkpoints: self.checkpoints.clone(),
            checkpoint_access: Arc::default(),
            label: self.checkpoint_label.clone(),
            stop: stop.clone(),
            sender,
            slots: Arc::new(Semaphore::new(concurrency * 2)),
        };
        let producer = tokio::task::spawn_blocking(move || source.run());
        let collected = self
            .collect(receiver, &mut states, concurrency, &stop, progress)
            .await;
        // Closing the receiver releases a blocked send. Wait for any current render to finish.
        stop.cancel();
        let source_result = producer.await.map_err(|source| Error::Task {
            context: "rendering pages".into(),
            source,
        })?;
        collected?;
        source_result?;
        Ok(RunReport {
            documents: states.into_iter().map(DocumentState::into_report).collect(),
            failed_inputs,
            cancelled: self.cancellation.is_cancelled(),
        })
    }

    async fn collect(
        &self,
        mut receiver: mpsc::Receiver<Message>,
        states: &mut [DocumentState],
        concurrency: usize,
        stop: &CancellationToken,
        progress: &dyn Progress,
    ) -> Result<()> {
        let mut tasks = JoinSet::<FinishedPage>::new();
        let mut closed = false;
        let mut cancelled = false;
        let mut fatal = None;
        while !closed || !tasks.is_empty() {
            tokio::select! {
                biased;
                () = stop.cancelled(), if !cancelled => {
                    cancelled = true;
                    closed = true;
                    receiver.close();
                    while receiver.try_recv().is_ok() {}
                }
                finished = tasks.join_next(), if !tasks.is_empty() => {
                    let result = match finished {
                        Some(Ok(page)) => states[page.document].recognized(page, &self.output, progress).await,
                        Some(Err(source)) => Err(Error::Task { context: "recognizing a page".into(), source }),
                        None => Ok(()),
                    };
                    if let Err(error) = result {
                        if fatal.is_none() { fatal = Some(error); }
                        stop.cancel();
                    }
                }
                message = receiver.recv(), if !closed && tasks.len() < concurrency => {
                    match message {
                        Some(Message::Started { document, pages, texts, checkpoint }) => {
                            states[document].started(pages, texts, checkpoint, progress);
                            states[document].finish_if_ready(&self.output, progress).await;
                        }
                        Some(Message::Page { document, page, image, permit }) => {
                            let recognizer = self.recognizer.clone();
                            let failed = states[document].failed.clone();
                            let stop = stop.clone();
                            tasks.spawn(async move {
                                let _permit = permit;
                                let result = if stop.is_cancelled() || failed.load(std::sync::atomic::Ordering::Acquire) {
                                    None
                                } else { Some(recognizer.recognize(&image).await) };
                                FinishedPage { document, page, result }
                            });
                        }
                        Some(Message::Failed { document, error }) => states[document].fail(error, progress),
                        None => closed = true,
                    }
                }
            }
        }
        fatal.map_or(Ok(()), Err)
    }

    async fn plan(&self, inputs: Vec<Input>) -> Result<(Vec<DocumentState>, Vec<FailedInput>)> {
        let mode = self.output_mode.clone();
        let writer = self.output.clone();
        let recursive = self.recursive;
        let skip_existing = self.skip_existing;
        tokio::task::spawn_blocking(move || {
            state::plan(&inputs, recursive, &mode, &writer, skip_existing)
        })
        .await
        .map_err(|source| Error::Task {
            context: "planning inputs".into(),
            source,
        })
    }
}
