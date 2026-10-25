//! Error types.

use std::fmt;

/// Boxed error used at extension points such as recognizers.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Result with [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors produced by this crate.
///
/// Messages leave out their underlying cause, which is available through
/// [`source`](std::error::Error::source); use [`ErrorChain`] (or `anyhow`'s `{:#}`) to print
/// the whole chain on one line.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Invalid page settings or recognizer concurrency.
    #[error("{0}")]
    InvalidOptions(String),
    /// No compatible `PDFium` runtime library could be loaded.
    #[error(
        "could not load PDFium; tried {attempted:?}; use the complete release archive, install the matching library from https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8111 beside the executable, or pass its path with --pdfium"
    )]
    PdfiumUnavailable {
        /// Library paths and search locations attempted, in order.
        attempted: Vec<String>,
        /// The last loader error.
        #[source]
        source: BoxError,
    },
    /// A PDF could not be opened or rendered.
    #[error("{context}")]
    Pdf {
        /// The operation and input, when applicable.
        context: String,
        /// The PDF renderer's error.
        #[source]
        source: pdfium_render::prelude::PdfiumError,
    },
    /// A rendered page's dimensions are invalid or exceed the image decoder's allocation limit.
    #[error("PDF page dimensions {width} × {height} pixels are unsupported; adjust the DPI")]
    PageDimensions {
        /// Requested width in pixels.
        width: f64,
        /// Requested height in pixels.
        height: f64,
    },
    /// No account or configuration remains usable for this run.
    ///
    /// A recognizer can return this error boxed to stop the whole pipeline. Other recognition
    /// errors fail only their document. Running recognitions finish before `run` returns.
    #[error("recognizer unavailable")]
    RecognizerUnavailable {
        /// The backend's reason for stopping the run.
        #[source]
        source: BoxError,
    },
    /// Rendering or encoding one page failed.
    #[error("preparing page {page}")]
    PagePreparation {
        /// Page number, starting at 1.
        page: usize,
        /// The page-source or encoder error.
        #[source]
        source: Box<Error>,
    },
    /// A page could not be recognized.
    #[error("recognizing page {page}")]
    Recognition {
        /// Page number, starting at 1.
        page: usize,
        /// The recognizer's error.
        #[source]
        source: BoxError,
    },
    /// An image could not be decoded or encoded.
    #[error("{context}")]
    Image {
        /// The operation and input, when applicable.
        context: String,
        /// The image codec error.
        #[source]
        source: image::ImageError,
    },
    /// The encoded page did not fit after reducing quality and dimensions.
    #[error("page image is {bytes} bytes after shrinking; recognizer limit is {limit} bytes")]
    ImageTooLarge {
        /// Final encoded size.
        bytes: usize,
        /// Largest allowed encoded size.
        limit: usize,
    },
    /// A checkpoint record could not be encoded.
    #[error("{context}")]
    Checkpoint {
        /// The operation that failed.
        context: String,
        /// The record encoder error.
        #[source]
        source: BoxError,
    },
    /// A blocking producer or recognition task failed unexpectedly.
    #[error("{context}")]
    Task {
        /// The task's operation.
        context: String,
        /// The task failure.
        #[source]
        source: tokio::task::JoinError,
    },

    /// An explicit input is not a supported document.
    #[error("unsupported input {path}; expected a PDF, JPEG or PNG file, or a directory")]
    UnsupportedInput {
        /// The input path.
        path: std::path::PathBuf,
    },
    /// Output could not be rendered.
    #[error("rendering {format} output")]
    Output {
        /// The output format.
        format: crate::Format,
        /// The underlying writer error.
        #[source]
        source: BoxError,
    },
    /// A file system operation failed.
    #[error("{context}")]
    Io {
        /// What was being done.
        context: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    /// Adds context to an I/O error.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// Displays an error followed by its causes, separated by `: `, like `anyhow`'s `{:#}`.
///
/// ```
/// use tahweel::{Error, ErrorChain};
///
/// let err = Error::io("writing a.txt", std::io::Error::other("disk full"));
/// assert_eq!(err.to_string(), "writing a.txt");
/// assert_eq!(ErrorChain(&err).to_string(), "writing a.txt: disk full");
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ErrorChain<'a>(pub &'a (dyn std::error::Error + 'static));

impl fmt::Display for ErrorChain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source = self.0.source();
        while let Some(cause) = source {
            write!(f, ": {cause}")?;
            source = cause.source();
        }
        Ok(())
    }
}
