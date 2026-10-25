use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageReader};

use crate::{Error, ImageFormat, PageImage, Result};

/// Whether page images retain their colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub enum ColorMode {
    /// One luminance channel.
    Grayscale,
    /// Three color channels.
    Color,
}

/// Encoding applied to every page, including image inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub enum ImageEncoding {
    /// JPEG, with a quality from 1 through 100.
    Jpeg {
        /// The initial quality, before any reduction needed to meet the recognizer's limit.
        quality: u8,
    },
    /// Lossless PNG with DEFLATE compression level 6.
    Png,
}

/// Page rendering and encoding settings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct PageOptions {
    /// PDF rendering resolution, in dots per inch. Defaults to 200.
    /// Bitmaps exceeding the image decoder's default allocation limit (512 MiB) are rejected.
    pub dpi: u32,
    /// Whether to retain color. Defaults to grayscale.
    pub color: ColorMode,
    /// Output image encoding. Defaults to lossless PNG.
    /// Oversized pages fall back to JPEG before reducing their dimensions.
    pub encoding: ImageEncoding,
    /// Maximum long edge for image inputs, in pixels. `None` keeps their original resolution.
    /// Defaults to 2,339 pixels. Images are never enlarged. PDF resolution is controlled by `dpi` instead.
    pub max_long_edge: Option<u32>,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            dpi: 200,
            color: ColorMode::Grayscale,
            encoding: ImageEncoding::Png,
            max_long_edge: Some(2_339),
        }
    }
}

impl PageOptions {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.dpi == 0 || self.max_long_edge == Some(0) {
            return Err(Error::InvalidOptions(
                "page dimensions and DPI must be positive".into(),
            ));
        }
        if let ImageEncoding::Jpeg { quality } = self.encoding
            && !(1..=100).contains(&quality)
        {
            return Err(Error::InvalidOptions(
                "JPEG quality must be between 1 and 100".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn read_image(
    path: &Path,
    options: &PageOptions,
    limit: Option<usize>,
) -> Result<PageImage> {
    let read_error = |source| Error::io(format!("reading {}", path.display()), source);
    let reader = ImageReader::open(path)
        .map_err(read_error)?
        .with_guessed_format()
        .map_err(read_error)?;
    let image_error = |source| Error::Image {
        context: format!("decoding {}", path.display()),
        source,
    };
    let mut decoder = reader.into_decoder().map_err(image_error)?;
    let orientation = decoder.orientation().map_err(image_error)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(image_error)?;
    image.apply_orientation(orientation);
    if let Some(edge) = options.max_long_edge
        && image.width().max(image.height()) > edge
    {
        image = image.resize(edge, edge, FilterType::Triangle);
    }
    encode(image, options, limit)
}

pub(crate) fn encode(
    mut image: DynamicImage,
    options: &PageOptions,
    limit: Option<usize>,
) -> Result<PageImage> {
    if image.color().has_alpha() {
        let mut background =
            image::RgbaImage::from_pixel(image.width(), image.height(), image::Rgba([255; 4]));
        image::imageops::overlay(&mut background, &image, 0, 0);
        image = DynamicImage::ImageRgba8(background);
    }
    image = match options.color {
        ColorMode::Grayscale => DynamicImage::ImageLuma8(image.into_luma8()),
        ColorMode::Color => DynamicImage::ImageRgb8(image.into_rgb8()),
    };
    let page = encode_once(&image, options.encoding)?;
    let Some(limit) = limit else {
        return Ok(page);
    };
    if page.data.len() <= limit {
        return Ok(page);
    }
    let mut bytes = page.data.len();
    drop(page);
    // Preserve dimensions first; never raise a caller's starting JPEG quality.
    for quality in [95, 85, 75, 60] {
        if let ImageEncoding::Jpeg { quality: initial } = options.encoding
            && quality >= initial
        {
            continue;
        }
        let page = encode_once(&image, ImageEncoding::Jpeg { quality })?;
        if page.data.len() <= limit {
            return Ok(page);
        }
        bytes = page.data.len();
    }
    shrink_to_limit(&image, bytes, limit)
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "byte lengths need only an approximate ratio; scaled dimensions are positive and at most the original u32 dimensions"
)]
fn shrink_to_limit(image: &DynamicImage, mut bytes: usize, limit: usize) -> Result<PageImage> {
    let (mut width, mut height) = (image.width(), image.height());
    let mut scale = 1.0;
    while width > 1 || height > 1 {
        scale *= (0.95 * (limit as f64 / bytes as f64).sqrt()).min(0.90);
        width = ((f64::from(image.width()) * scale) as u32).max(1);
        height = ((f64::from(image.height()) * scale) as u32).max(1);
        // Always sample the prepared source, preserving its aspect ratio across steps.
        let resized = image.resize_exact(width, height, FilterType::Triangle);
        let page = encode_once(&resized, ImageEncoding::Jpeg { quality: 75 })?;
        if page.data.len() <= limit {
            return Ok(page);
        }
        bytes = page.data.len();
    }
    Err(Error::ImageTooLarge { bytes, limit })
}

fn encode_once(image: &DynamicImage, encoding: ImageEncoding) -> Result<PageImage> {
    let mut data = Vec::new();
    let format = match encoding {
        ImageEncoding::Jpeg { quality } => {
            JpegEncoder::new_with_quality(&mut data, quality)
                .encode(
                    image.as_bytes(),
                    image.width(),
                    image.height(),
                    image.color().into(),
                )
                .map_err(|source| Error::Image {
                    context: "encoding JPEG page".into(),
                    source,
                })?;
            ImageFormat::Jpeg
        }
        ImageEncoding::Png => {
            PngEncoder::new_with_quality(
                &mut data,
                CompressionType::Level(6),
                PngFilterType::Adaptive,
            )
            .write_image(
                image.as_bytes(),
                image.width(),
                image.height(),
                image.color().into(),
            )
            .map_err(|source| Error::Image {
                context: "encoding PNG page".into(),
                source,
            })?;
            ImageFormat::Png
        }
    };
    Ok(PageImage::new(data, format, image.width(), image.height()))
}
mod pdf;

pub(crate) use pdf::{load_pdfium, render_pdf};
