//! Tahweel: convert PDFs and images into text with OCR.
//!
//! Inputs are PDF, JPEG and PNG files. Every page becomes an image that a [`Recognizer`] turns
//! into text, and the pages of each file are written as TXT, DOCX and JSON. Recognizers live in
//! their own crates: `tahweel-drive` runs OCR through Google Drive.

mod error;
mod recognize;

pub use error::{BoxError, Error, ErrorChain, Result};
pub use recognize::{ImageFormat, PageImage, Recognition, Recognizer};
