//! Google Drive OCR for [tahweel](https://docs.rs/tahweel).
//!
//! Each page image is uploaded as a Google Doc, which makes Drive run OCR, then exported as plain
//! text and deleted. Requests are authorized with service-account keys or through a Google
//! sign-in in the browser (OAuth).
