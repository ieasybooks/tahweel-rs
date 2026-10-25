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
