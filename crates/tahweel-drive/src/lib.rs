//! Google Drive OCR for [tahweel](https://docs.rs/tahweel).
//!
//! Each page image is uploaded as a Google Doc, which makes Drive run OCR, then exported as plain
//! text and deleted. Requests are authorized with service-account keys or through a Google
//! sign-in in the browser (OAuth).
//! Use [`DriveRecognizer::export_page_formats`] to compare raw native exports from the same
//! Google Doc before it is deleted, without changing normal plain-text recognition.
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
//!
//! For a browser sign-in, let the caller present the URL and open the browser. Reload saved tokens
//! with [`OAuthToken::load`]; the client refreshes expired tokens and saves updates
//! back to the same file. Use [`oauth::sign_out`] to remove that saved sign-in.
//!
//! ```no_run
//! use tahweel_drive::{DriveAuth, DriveConfig, DriveRecognizer, OAuthConfig, oauth::{self, SignIn}};
//!
//! # async fn example() -> Result<(), tahweel_drive::DriveError> {
//! let path = oauth::default_token_path()?;
//! let pending = SignIn::start(OAuthConfig::new(path)).await?;
//! // Present pending.authorization_url() in your application.
//! let token = pending.finish().await?;
//! let recognizer = DriveRecognizer::new(DriveConfig::new(DriveAuth::OAuth(token))).await?;
//! # Ok(())
//! # }
//! ```

mod auth;
mod config;
mod error;
pub mod oauth;
mod recognizer;
mod request;
mod scheduler;

pub use auth::ServiceAccountKey;
pub use config::{DriveAuth, DriveConfig};
pub use error::DriveError;
pub use recognizer::DriveRecognizer;

pub use oauth::{OAuthConfig, OAuthToken};
