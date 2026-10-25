use std::path::PathBuf;
use std::sync::Arc;

use crate::{Document, Error, Input};

/// Receives progress events. Closures implementing `Fn(E) + Send + Sync` work directly.
pub trait Progress<E = Event>: Send + Sync {
    /// Handles one event. Keep this callback short; it runs on the collecting task.
    fn report(&self, event: E);
}

impl<E, F: Fn(E) + Send + Sync> Progress<E> for F {
    fn report(&self, event: E) {
        self(event);
    }
}

/// Discards all progress events.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl<E> Progress<E> for NoProgress {
    fn report(&self, _event: E) {}
}

/// Progress of a whole [`crate::Tahweel`] run. Page numbers start at 1.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// An input could not be expanded into documents.
    InputFailed {
        /// The input as given.
        input: Input,
        /// Why discovery failed.
        error: Arc<Error>,
    },
    /// The document is open and any saved pages have been loaded.
    DocumentStarted {
        /// The document, including its planned output stem.
        document: Document,
        /// Total number of pages.
        pages: usize,
        /// Pages loaded from a checkpoint.
        resumed: usize,
    },
    /// A page was recognized and saved to its checkpoint, when enabled.
    PageRecognized {
        /// The document.
        document: Document,
        /// Page number, starting at 1.
        page: usize,
    },
    /// Every requested output for this document already exists.
    DocumentSkipped {
        /// The document.
        document: Document,
    },
    /// All pages were recognized and every output was written.
    DocumentFinished {
        /// The document.
        document: Document,
        /// Number of pages.
        pages: usize,
        /// Written files, in format order.
        outputs: Vec<PathBuf>,
    },
    /// The document failed; other documents can continue.
    DocumentFailed {
        /// The document.
        document: Document,
        /// Why it failed.
        error: Arc<Error>,
    },
}
