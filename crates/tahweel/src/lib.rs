//! Tahweel: convert PDFs and images into text with OCR.
//!
//! Inputs are PDF, JPEG and PNG files. Every page becomes an image that a [`Recognizer`] turns
//! into text, and the pages of each file are written as TXT, DOCX and JSON. Recognizers live in
//! their own crates: `tahweel-drive` runs OCR through Google Drive.
//!
//! PDF inputs need a compatible `PDFium` shared library at runtime. Put it beside the executable,
//! install it on the system library search path, or set [`TahweelBuilder::pdfium_library`].
//! Compiling the crate and converting images require no `PDFium` installation. The native library
//! is initialized once per process by `pdfium-render`; later runs reuse those bindings.
//!
//! ```no_run
//! # async fn example(recognizer: impl tahweel::Recognizer + 'static) -> tahweel::Result<()> {
//! use tahweel::{Format, Input, NoProgress, OutputMode, OutputWriter, Tahweel};
//!
//! let pipeline = Tahweel::builder(recognizer)
//!     .output(OutputWriter::new().formats([Format::Txt, Format::Json]))
//!     .output_mode(OutputMode::TreeToTree("text".into()))
//!     .recursive(true)
//!     .checkpoints("checkpoints")
//!     .checkpoint_label("my-recognizer:ar")
//!     .build();
//! let report = pipeline.run([Input::from("books")], &NoProgress).await?;
//! assert_eq!(report.failures(), 0);
//! # Ok(())
//! # }
//! ```

mod checkpoint;
mod error;
mod event;
mod input;
mod output;
mod page;
mod pipeline;
mod recognize;

pub use error::{BoxError, Error, ErrorChain, Result};
pub use event::{Event, NoProgress, Progress};
pub use input::{Document, DocumentKind, Input};
pub use output::{Format, OutputMode, OutputWriter};
pub use page::{ColorMode, ImageEncoding, PageOptions};
pub use pipeline::{
    DocumentReport, DocumentStatus, FailedInput, RunReport, Tahweel, TahweelBuilder,
};
pub use recognize::{ImageFormat, PageImage, Recognition, Recognizer};

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
