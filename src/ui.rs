//! Terminal progress and log output.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use tahweel::{Document, DocumentStatus, ErrorChain, Event, RunReport};
use tracing_subscriber::fmt::MakeWriter;

/// Progress bars plus result lines on stderr.
pub(crate) struct Ui {
    multi: MultiProgress,
    overall: ProgressBar,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    documents: HashMap<(PathBuf, String), ProgressBar>,
    known: u64,
    finished: u64,
}

impl Ui {
    /// Draws bars on stderr unless quiet or stderr is not a terminal.
    pub(crate) fn new(quiet: bool) -> Self {
        let target = if quiet {
            ProgressDrawTarget::hidden()
        } else {
            ProgressDrawTarget::stderr()
        };
        let multi = MultiProgress::with_draw_target(target);
        let overall = multi.add(ProgressBar::new(0));
        overall.set_style(
            ProgressStyle::with_template(
                "Total {bar:24.cyan/blue} {pos}/{len} pages · {msg} [{elapsed_precise}]",
            )
            .expect("template")
            .progress_chars("━╸ "),
        );
        overall.set_message("opening documents");
        overall.enable_steady_tick(Duration::from_millis(250));
        Self {
            multi,
            overall,
            state: Mutex::new(State::default()),
        }
    }

    pub(crate) fn log_writer(&self) -> LogWriter {
        LogWriter(self.multi.clone())
    }

    pub(crate) fn line(&self, text: impl AsRef<str>) {
        if self.multi.is_hidden() {
            eprintln!("{}", text.as_ref());
        } else {
            let _ = self.multi.println(text.as_ref());
        }
    }

    pub(crate) fn clear(&self) {
        self.overall.finish_and_clear();
        for (_, bar) in self.lock().documents.drain() {
            bar.finish_and_clear();
        }
        let _ = self.multi.clear();
    }

    pub(crate) fn handle(&self, event: Event) {
        let mut state = self.lock();
        let line = match event {
            Event::InputFailed { error, .. } => Some(format!("✗ {}", ErrorChain(&*error))),
            Event::DocumentStarted {
                document,
                pages,
                resumed,
            } => {
                let bar = self.multi.add(ProgressBar::new(pages as u64));
                bar.set_style(
                    ProgressStyle::with_template(
                        "{prefix} {bar:24.cyan/blue} {pos}/{len} pages {msg}",
                    )
                    .expect("template")
                    .progress_chars("━╸ "),
                );
                bar.set_prefix(truncate(&document.title, 40));
                bar.set_position(resumed as u64);
                if resumed > 0 {
                    bar.set_message(format!("({resumed} resumed)"));
                }
                state.documents.insert(key(&document), bar);
                state.known += 1;
                // The core reports page totals when documents open, so this total grows.
                self.overall.inc_length(pages as u64);
                self.overall.inc(resumed as u64);
                None
            }
            Event::PageRecognized { document, .. } => {
                if let Some(bar) = state.documents.get(&key(&document)) {
                    bar.inc(1);
                }
                self.overall.inc(1);
                None
            }
            Event::DocumentSkipped { document } => {
                state.finish(&document);
                Some(format!("↷ {} (outputs exist)", document.path.display()))
            }
            Event::DocumentFinished {
                document, pages, ..
            } => {
                state.finish(&document);
                Some(format!("✓ {} · {pages} pages", document.path.display()))
            }
            Event::DocumentFailed { document, error } => {
                state.finish(&document);
                Some(format!(
                    "✗ {}: {}",
                    document.path.display(),
                    ErrorChain(&*error)
                ))
            }
            _ => None,
        };
        self.overall
            .set_message(format!("{}/{} documents", state.finished, state.known));
        drop(state);
        if let Some(line) = line {
            self.line(line);
        }
    }

    /// Prints the final totals, including unfinished documents after Ctrl-C.
    pub(crate) fn summary(&self, report: &RunReport) {
        self.clear();
        let mut converted = 0;
        let mut skipped = 0;
        let mut cancelled = 0;
        for document in &report.documents {
            match document.status {
                DocumentStatus::Converted { .. } => converted += 1,
                DocumentStatus::Skipped => skipped += 1,
                DocumentStatus::Cancelled => cancelled += 1,
                _ => {}
            }
        }
        let mut text = format!("{converted} converted");
        for (count, label) in [
            (skipped, "skipped"),
            (report.failures(), "failed"),
            (cancelled, "cancelled"),
        ] {
            if count > 0 {
                let _ = write!(text, ", {count} {label}");
            }
        }
        self.line(text);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn finish(&mut self, document: &Document) {
        if let Some(bar) = self.documents.remove(&key(document)) {
            bar.finish_and_clear();
        } else {
            // Skipped documents and failures before opening do not emit DocumentStarted.
            self.known += 1;
        }
        self.finished += 1;
    }
}

fn key(document: &Document) -> (PathBuf, String) {
    (document.path.clone(), document.id.clone())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

/// Writes log lines above the progress bars.
#[derive(Clone)]
pub(crate) struct LogWriter(MultiProgress);

impl<'a> MakeWriter<'a> for LogWriter {
    type Writer = LogLine;

    fn make_writer(&'a self) -> Self::Writer {
        LogLine {
            multi: self.0.clone(),
            buffer: Vec::new(),
        }
    }
}

pub(crate) struct LogLine {
    multi: MultiProgress,
    buffer: Vec<u8>,
}

impl Write for LogLine {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for LogLine {
    fn drop(&mut self) {
        if !self.buffer.is_empty() {
            let buffer = std::mem::take(&mut self.buffer);
            self.multi.suspend(|| {
                let _ = std::io::stderr().write_all(&buffer);
            });
        }
    }
}
