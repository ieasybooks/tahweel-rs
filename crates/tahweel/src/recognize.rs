//! Recognizing the text on page images.

use std::sync::Arc;

use async_trait::async_trait;

use crate::BoxError;

/// Recognizes the text on one page image at a time, for example a cloud OCR service.
#[async_trait]
pub trait Recognizer: Send + Sync {
    /// Recognizes the text on `page`. Implementations handle their own retries, rate limits
    /// and cleanup.
    async fn recognize(&self, page: &PageImage) -> Result<Recognition, BoxError>;

    /// Largest encoded image accepted per request, in bytes.
    fn max_image_bytes(&self) -> Option<usize> {
        None
    }

    /// How many requests may usefully be in flight at once.
    fn max_in_flight(&self) -> usize {
        1
    }
}

#[async_trait]
impl<T: Recognizer + ?Sized> Recognizer for Arc<T> {
    async fn recognize(&self, page: &PageImage) -> Result<Recognition, BoxError> {
        (**self).recognize(page).await
    }

    fn max_image_bytes(&self) -> Option<usize> {
        (**self).max_image_bytes()
    }

    fn max_in_flight(&self) -> usize {
        (**self).max_in_flight()
    }
}

/// An encoded page image, ready to send to a recognizer.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PageImage {
    /// The encoded image.
    pub data: Vec<u8>,
    /// How `data` is encoded.
    pub format: ImageFormat,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl PageImage {
    /// An encoded image of `width` × `height` pixels.
    pub fn new(data: Vec<u8>, format: ImageFormat, width: u32, height: u32) -> Self {
        Self {
            data,
            format,
            width,
            height,
        }
    }
}

impl std::fmt::Debug for PageImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageImage")
            .field("bytes", &self.data.len())
            .field("format", &self.format)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

/// How a page image is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFormat {
    /// JPEG.
    Jpeg,
    /// PNG.
    Png,
}

impl ImageFormat {
    /// The lowercase file extension, without a dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }

    /// The media type, e.g. `image/jpeg`.
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }
}

/// What a recognizer read on one page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Recognition {
    /// The page's text, lines separated by `\n`.
    pub text: String,
}

impl Recognition {
    /// A recognition with only text.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}
