use std::{fmt, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use reqwest::{Method, RequestBuilder};
use serde::Deserialize;
use tahweel::{BoxError, PageImage, Recognition, Recognizer};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::auth::Credential;
use crate::request::{self, Retry};
use crate::scheduler::{Scheduler, Slot};
use crate::{DriveAuth, DriveConfig, DriveError};

/// A shared Google Drive OCR client with per-account scheduling and token caching.
///
/// Dropping a recognition after upload schedules a best-effort delete on the current
/// Tokio runtime. Keep that runtime alive for cleanup; graceful callers should await
/// in-flight recognitions. Uploads are tagged so a later client can remove files left
/// behind by interrupted runs or lost responses once they are more than an hour old.
#[derive(Clone)]
pub struct DriveRecognizer {
    inner: Arc<Inner>,
}

struct Inner {
    client: reqwest::Client,
    config: DriveConfig,
    credentials: Vec<Credential>,
    scheduler: Arc<Scheduler>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    Upload,
    File,
    Cleanup,
}

impl fmt::Debug for DriveRecognizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DriveRecognizer")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl DriveRecognizer {
    /// Validates settings and fetches an initial token for every account before any OCR.
    /// Also attempts to delete this app's tagged files older than one hour. Cleanup
    /// failures are logged and do not prevent recognition.
    pub async fn new(config: DriveConfig) -> Result<Self, DriveError> {
        config.validate()?;
        let client = request::client(config.request_timeout)?;
        let credentials: Vec<_> = match &config.auth {
            DriveAuth::ServiceAccounts(keys) => keys.iter().cloned().map(Credential::new).collect(),
            DriveAuth::OAuth(token) => vec![Credential::oauth(token.clone())?],
        };
        let scheduler = Scheduler::new(credentials.len(), config.concurrency_per_account);
        let recognizer = Self {
            inner: Arc::new(Inner {
                client,
                config,
                credentials,
                scheduler,
            }),
        };
        for (account, credential) in recognizer.inner.credentials.iter().enumerate() {
            credential
                .token(
                    &recognizer.inner.client,
                    &recognizer.inner.config,
                    None,
                    true,
                )
                .await?;
            recognizer.inner.cleanup_leftovers(account).await;
        }
        Ok(recognizer)
    }

    /// Initial configuration. Refreshed tokens are kept internally and written to their
    /// saved file; credentials remain redacted in `Debug`.
    pub fn config(&self) -> &DriveConfig {
        &self.inner.config
    }

    /// Recognizes one encoded image, returning a typed Drive error.
    pub async fn recognize_page(&self, page: &PageImage) -> Result<Recognition, DriveError> {
        let mut file = self.upload_page(page).await?;
        let result = self
            .inner
            .export(file.account(), &file.id, "text/plain")
            .await;
        file.cleanup().await;
        let bytes = result?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| DriveError::InvalidResponse("export was not UTF-8 text"))?;
        Ok(Recognition::new(clean_text(text)))
    }

    /// Uploads one image and exports the same Google Doc in each requested MIME type.
    ///
    /// This is useful for comparing Drive's native export formats. Each result contains the
    /// exact response bytes, with no text decoding or cleanup, and corresponds to the MIME
    /// type at the same index. An export failure is retained while subsequent formats are
    /// still attempted. The temporary document is deleted after all exports, using the same
    /// best-effort cleanup as [`Self::recognize_page`].
    ///
    /// An empty format list or blank MIME type is rejected before uploading. The outer error
    /// reports validation or upload failure; individual export errors are in the returned list.
    pub async fn export_page_formats(
        &self,
        page: &PageImage,
        mime_types: &[&str],
    ) -> Result<Vec<Result<Bytes, DriveError>>, DriveError> {
        if mime_types.is_empty() || mime_types.iter().any(|mime| mime.trim().is_empty()) {
            return Err(DriveError::Config(
                "at least one nonempty export MIME type is required".into(),
            ));
        }
        let mut file = self.upload_page(page).await?;
        let mut exports = Vec::with_capacity(mime_types.len());
        for mime_type in mime_types {
            exports.push(self.inner.export(file.account(), &file.id, mime_type).await);
        }
        file.cleanup().await;
        Ok(exports)
    }

    async fn upload_page(&self, page: &PageImage) -> Result<UploadedFile, DriveError> {
        if page.data.len() > self.inner.config.max_image_bytes {
            return Err(DriveError::ImageTooLarge {
                bytes: page.data.len(),
                limit: self.inner.config.max_image_bytes,
            });
        }
        let (body, content_type) = multipart(page);
        loop {
            let slot = self.inner.scheduler.acquire().await?;
            let upload = self
                .inner
                .upload(slot.index, body.clone(), &content_type)
                .await;
            let id = match upload {
                Ok(Some(id)) => id,
                Ok(None) => continue,
                Err(error)
                    if self.inner.credentials[slot.index].is_service_account()
                        && error.is_storage_quota() =>
                {
                    if self.inner.scheduler.disable(slot.index) {
                        tracing::warn!(
                            account = self.inner.credentials[slot.index].name(),
                            "Google Drive account has no storage; disabling its lane"
                        );
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            // No await between learning the file ID and installing the cleanup guard.
            return Ok(UploadedFile {
                inner: self.inner.clone(),
                slot: Some(slot),
                id,
            });
        }
    }
}

#[async_trait]
impl Recognizer for DriveRecognizer {
    async fn recognize(&self, page: &PageImage) -> Result<Recognition, BoxError> {
        self.recognize_page(page).await.map_err(|error| {
            if error.is_run_level() {
                Box::new(tahweel::Error::RecognizerUnavailable {
                    source: Box::new(error),
                }) as BoxError
            } else {
                Box::new(error) as BoxError
            }
        })
    }

    fn max_image_bytes(&self) -> Option<usize> {
        Some(self.inner.config.max_image_bytes)
    }

    fn max_in_flight(&self) -> usize {
        self.inner.credentials.len() * self.inner.config.concurrency_per_account
    }
}

impl Inner {
    async fn request(
        &self,
        account: usize,
        request: RequestBuilder,
        kind: RequestKind,
    ) -> Result<Option<Bytes>, DriveError> {
        // Only an upload may be skipped when another page disables its lane.
        let credential = &self.credentials[account];
        let remember_rejection = kind != RequestKind::Cleanup;
        let mut retry = Retry::new(&self.config);
        let mut refreshed = false;
        loop {
            self.scheduler.wait_hold(account).await;
            if kind == RequestKind::Upload && !self.scheduler.enabled(account) {
                return Ok(None);
            }
            let token = match credential
                .token(&self.client, &self.config, None, remember_rejection)
                .await
            {
                Ok(token) => token,
                Err(error) => {
                    self.scheduler.failed(account, &error);
                    if remember_rejection && error.is_run_level() {
                        self.scheduler.reject_credentials();
                    }
                    return Err(error);
                }
            };
            // A page using the previous token may have placed a hold during refresh.
            self.scheduler.wait_hold(account).await;
            let result = request::send(
                request
                    .try_clone()
                    .expect("Drive request body is cloneable")
                    .bearer_auth(&token.value),
            )
            .await;
            let error = match result {
                Ok(bytes) => {
                    self.scheduler.succeeded(account);
                    return Ok(Some(bytes));
                }
                Err(error) => error,
            };
            self.scheduler.failed(account, &error);
            if error.is_unauthorized() {
                if refreshed {
                    if remember_rejection {
                        self.scheduler.reject_credentials();
                    }
                    return Err(DriveError::Authentication(Box::new(error)));
                }
                if let Err(error) = credential
                    .token(
                        &self.client,
                        &self.config,
                        Some(token.generation),
                        remember_rejection,
                    )
                    .await
                {
                    self.scheduler.failed(account, &error);
                    if remember_rejection && error.is_run_level() {
                        self.scheduler.reject_credentials();
                    }
                    return Err(error);
                }
                refreshed = true;
                continue;
            }
            let Some(delay) = retry.delay(&error) else {
                return Err(error);
            };
            if error.is_rate_limited() || error.retry_after().is_some() {
                self.scheduler.hold(account, delay)?;
            } else {
                tokio::time::sleep(delay).await;
            }
        }
    }

    async fn upload(
        &self,
        account: usize,
        body: Bytes,
        content_type: &str,
    ) -> Result<Option<String>, DriveError> {
        #[derive(Deserialize)]
        struct Uploaded {
            id: String,
        }
        let mut request = self
            .client
            .post(format!(
                "{}/drive/v3/files",
                self.config.upload_base.trim_end_matches('/')
            ))
            .query(&[("uploadType", "multipart"), ("fields", "id")])
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body);
        if let Some(language) = &self.config.language {
            request = request.query(&[("ocrLanguage", language)]);
        }
        let Some(bytes) = self.request(account, request, RequestKind::Upload).await? else {
            return Ok(None);
        };
        let uploaded: Uploaded = serde_json::from_slice(&bytes)
            .map_err(|_| DriveError::InvalidResponse("upload did not return a file ID"))?;
        if uploaded.id.is_empty() {
            return Err(DriveError::InvalidResponse(
                "upload returned an empty file ID",
            ));
        }
        Ok(Some(uploaded.id))
    }

    fn file_request(&self, method: Method, id: &str, export: bool) -> RequestBuilder {
        let mut url = reqwest::Url::parse(&format!(
            "{}/drive/v3/files/",
            self.config.api_base.trim_end_matches('/')
        ))
        .expect("configuration URLs are validated");
        {
            let mut segments = url
                .path_segments_mut()
                .expect("HTTP URLs support path segments");
            segments.pop_if_empty().push(id);
            if export {
                segments.push("export");
            }
        }
        self.client.request(method, url)
    }

    async fn export(&self, account: usize, id: &str, mime_type: &str) -> Result<Bytes, DriveError> {
        let request = self
            .file_request(Method::GET, id, true)
            .query(&[("mimeType", mime_type)]);
        let bytes = self
            .request(account, request, RequestKind::File)
            .await?
            .expect("only uploads can be skipped");
        Ok(bytes)
    }

    async fn delete(&self, account: usize, id: &str, kind: RequestKind) -> bool {
        let request = self.file_request(Method::DELETE, id, false);
        if let Err(error) = self.request(account, request, kind).await {
            // A repeated delete after cancellation can find the file already gone.
            if !matches!(error, DriveError::Status { status: 404, .. }) {
                tracing::warn!(file_id = id, error = %error, "could not delete temporary Google Drive file");
                return false;
            }
        }
        true
    }

    async fn cleanup_leftovers(&self, account: usize) {
        let mut deleted = 0;
        if let Err(error) = self.sweep_leftovers(account, &mut deleted).await {
            tracing::warn!(
                account = self.credentials[account].name(),
                error = %error,
                "could not list leftover Google Drive files"
            );
        }
        if deleted > 0 {
            tracing::warn!(
                account = self.credentials[account].name(),
                count = deleted,
                "deleted leftover Google Drive files"
            );
        }
    }

    async fn sweep_leftovers(&self, account: usize, deleted: &mut usize) -> Result<(), DriveError> {
        #[derive(Deserialize)]
        struct File {
            id: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            next_page_token: Option<String>,
            #[serde(default)]
            files: Vec<File>,
        }

        let cutoff = (OffsetDateTime::now_utc() - time::Duration::hours(1))
            .format(&Rfc3339)
            .map_err(|_| DriveError::Config("could not format the cleanup cutoff time".into()))?;
        let query = format!(
            "appProperties has {{ key='tahweel' and value='ocr' }} \
             and createdTime < '{cutoff}' and trashed = false"
        );
        let mut page_token = None;
        loop {
            let mut request = self
                .client
                .get(format!(
                    "{}/drive/v3/files",
                    self.config.api_base.trim_end_matches('/')
                ))
                .query(&[("q", query.as_str()), ("fields", "nextPageToken,files(id)")]);
            if let Some(token) = &page_token {
                request = request.query(&[("pageToken", token)]);
            }
            let bytes = self
                .request(account, request, RequestKind::Cleanup)
                .await?
                .expect("only uploads can be skipped");
            let page: Page = serde_json::from_slice(&bytes)
                .map_err(|_| DriveError::InvalidResponse("invalid file list"))?;
            for file in page.files {
                if file.id.is_empty() {
                    return Err(DriveError::InvalidResponse(
                        "file list returned an empty ID",
                    ));
                }
                if self.delete(account, &file.id, RequestKind::Cleanup).await {
                    *deleted += 1;
                }
            }
            page_token = page.next_page_token.filter(|token| !token.is_empty());
            if page_token.is_none() {
                return Ok(());
            }
        }
    }
}

struct UploadedFile {
    inner: Arc<Inner>,
    slot: Option<Slot>,
    id: String,
}

impl UploadedFile {
    fn account(&self) -> usize {
        self.slot
            .as_ref()
            .expect("file owns its account until cleanup")
            .index
    }

    async fn cleanup(&mut self) {
        self.inner
            .delete(self.account(), &self.id, RequestKind::File)
            .await;
        self.slot.take();
    }
}

impl Drop for UploadedFile {
    fn drop(&mut self) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                file_id = self.id,
                "no Tokio runtime available to delete temporary Google Drive file"
            );
            return;
        };
        let inner = self.inner.clone();
        let id = self.id.clone();
        runtime.spawn(async move {
            inner.delete(slot.index, &id, RequestKind::File).await;
            drop(slot);
        });
    }
}

fn multipart(page: &PageImage) -> (Bytes, String) {
    let boundary = format!("tahweel-{:032x}", rand::random::<u128>());
    let name = format!("tahweel-{:032x}", rand::random::<u128>());
    let metadata = serde_json::json!({
        "name": name,
        "mimeType": "application/vnd.google-apps.document",
        "appProperties": {"tahweel": "ocr"},
    });
    let metadata_part = format!(
        "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n"
    );
    let image_type = page.format.mime_type();
    let image_header = format!("--{boundary}\r\nContent-Type: {image_type}\r\n\r\n");
    let mut body = Vec::with_capacity(page.data.len() + 512);
    body.extend_from_slice(metadata_part.as_bytes());
    body.extend_from_slice(image_header.as_bytes());
    body.extend_from_slice(&page.data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (
        Bytes::from(body),
        format!("multipart/related; boundary={boundary}"),
    )
}

fn clean_text(text: &str) -> String {
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let content = text
        .strip_prefix("________________")
        .filter(|rest| rest.is_empty() || rest.starts_with('\n'))
        .unwrap_or(&text);
    content.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::clean_text;

    #[test]
    fn only_removes_the_leading_drive_header() {
        assert_eq!(
            clean_text("\u{feff}________________\r\n\r\n بسم الله\r\nسطر\rآخر \r\n"),
            "بسم الله\nسطر\nآخر"
        );
        assert_eq!(
            clean_text("\u{feff}hello\n________________\n\nend"),
            "hello\n________________\n\nend"
        );
        assert_eq!(
            clean_text("________________content"),
            "________________content"
        );
        assert_eq!(
            clean_text("_________________\ncontent"),
            "_________________\ncontent"
        );
    }

    #[test]
    fn removes_header_with_any_number_of_line_breaks_and_handles_blank_pages() {
        for text in [
            "",
            "\u{feff}",
            "\r\n",
            "________________",
            "\u{feff}________________",
            "________________\n",
            "________________\r\n\r\n\r\n",
        ] {
            assert_eq!(clean_text(text), "", "{text:?}");
        }
        for breaks in ["\n", "\n\n", "\n\n\n", "\r", "\r\n", "\r\n\r\n\r\n"] {
            assert_eq!(
                clean_text(&format!("\u{feff}________________{breaks}نص")),
                "نص"
            );
        }
    }
}
