use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::checkpoint::Checkpoint;
use crate::output::{Layout, Target};
use crate::{
    BoxError, Document, Error, Event, Input, OutputMode, OutputWriter, Progress, Recognition,
    Result,
};

use super::{DocumentReport, DocumentStatus, FailedInput};

pub(super) struct FinishedPage {
    pub(super) document: usize,
    pub(super) page: usize,
    pub(super) result: Option<std::result::Result<Recognition, BoxError>>,
}

pub(super) struct DocumentState {
    pub(super) document: Document,
    pub(super) status: Option<DocumentStatus>,
    pub(super) failed: Arc<AtomicBool>,
    target: Option<Target>,
    total: Option<usize>,
    texts: BTreeMap<usize, String>,
    checkpoint: Option<Checkpoint>,
    remaining: Arc<AtomicUsize>,
}

impl DocumentState {
    pub(super) fn report_planned(&self, progress: &dyn Progress) {
        match &self.status {
            Some(DocumentStatus::Skipped) => progress.report(Event::DocumentSkipped {
                document: self.document.clone(),
            }),
            Some(DocumentStatus::Failed(error)) => progress.report(Event::DocumentFailed {
                document: self.document.clone(),
                error: error.clone(),
            }),
            _ => {}
        }
    }

    pub(super) fn started(
        &mut self,
        pages: usize,
        texts: BTreeMap<usize, String>,
        checkpoint: Option<Checkpoint>,
        progress: &dyn Progress,
    ) {
        self.total = Some(pages);
        self.texts = texts;
        self.checkpoint = checkpoint;
        progress.report(Event::DocumentStarted {
            document: self.document.clone(),
            pages,
            resumed: self.texts.len(),
        });
    }

    pub(super) fn fail(&mut self, error: Error, progress: &dyn Progress) {
        if self.status.is_none() {
            let error = Arc::new(error);
            self.status = Some(DocumentStatus::Failed(error.clone()));
            self.failed.store(true, Ordering::Release);
            self.texts.clear();
            progress.report(Event::DocumentFailed {
                document: self.document.clone(),
                error,
            });
        }
    }

    pub(super) async fn recognized(
        &mut self,
        page: FinishedPage,
        writer: &OutputWriter,
        progress: &dyn Progress,
    ) -> Result<()> {
        let Some(result) = page.result else {
            return Ok(());
        };
        let text = match result {
            Ok(recognition) => recognition.text,
            Err(source) => {
                let fatal = recognizer_unavailable(source.as_ref());
                let error = Error::Recognition {
                    page: page.page,
                    source,
                };
                if fatal {
                    return Err(error);
                }
                self.fail(error, progress);
                return Ok(());
            }
        };
        if let Some(checkpoint) = &self.checkpoint
            && let Err(error) = checkpoint.append(page.page, &text).await
        {
            self.fail(error, progress);
            return Ok(());
        }
        if self.status.is_none() {
            self.texts.insert(page.page, text);
        }
        progress.report(Event::PageRecognized {
            document: self.document.clone(),
            page: page.page,
        });
        self.finish_if_ready(writer, progress).await;
        Ok(())
    }

    pub(super) async fn finish_if_ready(&mut self, writer: &OutputWriter, progress: &dyn Progress) {
        if self.status.is_some() || self.total != Some(self.texts.len()) {
            return;
        }
        let Some(target) = self.target.take() else {
            return;
        };
        let pages: Vec<_> = std::mem::take(&mut self.texts).into_values().collect();
        let writer = writer.clone();
        let written = tokio::task::spawn_blocking(move || {
            let outputs = writer.write(&target.dir, &target.name, &pages)?;
            Ok::<_, Error>((pages, outputs))
        })
        .await;
        match written {
            Ok(Ok((pages, outputs))) => {
                // Repeated inputs share a checkpoint. Delete it once every occurrence has
                // written its outputs; failed or cancelled occurrences keep their saved pages.
                if self.remaining.fetch_sub(1, Ordering::AcqRel) == 1
                    && let Some(checkpoint) = &self.checkpoint
                    && let Err(error) = checkpoint.remove().await
                {
                    self.fail(error, progress);
                    return;
                }
                progress.report(Event::DocumentFinished {
                    document: self.document.clone(),
                    pages: pages.len(),
                    outputs: outputs.clone(),
                });
                self.status = Some(DocumentStatus::Converted { pages, outputs });
            }
            Ok(Err(error)) => self.fail(error, progress),
            Err(source) => self.fail(
                Error::Task {
                    context: "writing outputs".into(),
                    source,
                },
                progress,
            ),
        }
    }

    pub(super) fn into_report(self) -> DocumentReport {
        DocumentReport {
            document: self.document,
            status: self.status.unwrap_or(DocumentStatus::Cancelled),
        }
    }
}

pub(super) fn plan(
    inputs: &[Input],
    recursive: bool,
    mode: &OutputMode,
    writer: &OutputWriter,
    skip: bool,
) -> (Vec<DocumentState>, Vec<FailedInput>) {
    let mut layout = Layout::new(mode.clone(), writer.clone(), inputs);
    let mut failed_inputs = Vec::new();
    let mut discovered = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        match input.discover(recursive, mode.dir()) {
            Ok(documents) => {
                for document in documents {
                    let directory = layout.directory(&document, index);
                    let reserved = layout
                        .reserve_input(&document.path)
                        .and_then(|()| layout.require_directory(&directory));
                    discovered.push((document, reserved));
                }
            }
            Err(error) => failed_inputs.push(FailedInput {
                input: input.clone(),
                error: Arc::new(error),
            }),
        }
    }
    let mut states: Vec<_> = discovered
        .into_iter()
        .map(|(mut document, directory)| {
            let planned = directory.and_then(|directory| layout.plan(&document, &directory));
            let (target, status) = match planned {
                Ok(target) => {
                    document.id.clone_from(&target.name);
                    let status = (skip && target.exists(writer)).then_some(DocumentStatus::Skipped);
                    (Some(target), status)
                }
                Err(error) => (None, Some(DocumentStatus::Failed(Arc::new(error)))),
            };
            DocumentState {
                document,
                status,
                target,
                failed: Arc::new(AtomicBool::new(false)),
                total: None,
                texts: BTreeMap::new(),
                checkpoint: None,
                remaining: Arc::new(AtomicUsize::new(1)),
            }
        })
        .collect();
    let mut groups = HashMap::<_, Arc<AtomicUsize>>::new();
    for state in &mut states {
        if state.status.is_none() {
            let canonical = dunce::canonicalize(&state.document.path)
                .unwrap_or_else(|_| state.document.path.clone());
            state.remaining = groups.entry(canonical).or_default().clone();
            state.remaining.fetch_add(1, Ordering::Relaxed);
        }
    }
    (states, failed_inputs)
}

fn recognizer_unavailable(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if matches!(
            error.downcast_ref::<Error>(),
            Some(Error::RecognizerUnavailable { .. })
        ) {
            return true;
        }
        cause = error.source();
    }
    false
}
