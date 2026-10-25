//! Google Drive OCR for [tahweel](https://docs.rs/tahweel).
//!
//! Each page image is uploaded as a Google Doc, which makes Drive run OCR, then exported as plain
//! text and deleted. Requests are authorized with service-account keys or through a Google
//! sign-in in the browser (OAuth).
//!
//! ```no_run
//! use tahweel_drive::{DriveAuth, DriveConfig, DriveRecognizer, ServiceAccountKey};
//!
//! # async fn example() -> Result<(), tahweel_drive::DriveError> {
//! let key = ServiceAccountKey::from_file("service-account.json")?;
//! let recognizer = DriveRecognizer::new(DriveConfig::new(
//!     DriveAuth::ServiceAccounts(vec![key]),
//! )).await?;
//! # Ok(())
//! # }
//! ```

mod auth;
mod config;
mod error;
mod recognizer;
mod request;
mod scheduler;

pub use auth::ServiceAccountKey;
pub use config::{DriveAuth, DriveConfig};
pub use error::DriveError;
pub use recognizer::DriveRecognizer;
