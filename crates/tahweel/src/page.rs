use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader};

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
    /// Lossless PNG.
    Png,
}

/// Page rendering and encoding settings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct PageOptions {
    /// PDF rendering resolution, in dots per inch.
    /// Bitmaps exceeding the image decoder's default allocation limit (512 MiB) are rejected.
    pub dpi: u32,
    /// Whether to retain color.
    pub color: ColorMode,
    /// Output image encoding.
    pub encoding: ImageEncoding,
    /// Maximum long edge for image inputs, in pixels. `None` keeps their original resolution.
    /// Images are never enlarged. PDF resolution is controlled by `dpi` instead.
    pub max_long_edge: Option<u32>,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            dpi: 200,
            color: ColorMode::Grayscale,
            encoding: ImageEncoding::Jpeg { quality: 85 },
            max_long_edge: None,
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
    let mut encoding = options.encoding;
    let page = encode_once(&image, encoding)?;
    let Some(limit) = limit else {
        return Ok(page);
    };
    if page.data.len() <= limit {
        return Ok(page);
    }
    let mut bytes = page.data.len();
    drop(page);
    // Try a few fixed quality levels before reducing the page's dimensions.
    if let ImageEncoding::Jpeg { quality } = encoding {
        for lower in [70, 55, 40] {
            if lower >= quality {
                continue;
            }
            encoding = ImageEncoding::Jpeg { quality: lower };
            let page = encode_once(&image, encoding)?;
            if page.data.len() <= limit {
                return Ok(page);
            }
            bytes = page.data.len();
        }
    }
    for _ in 0..6 {
        let width = (image.width() / 4 * 3 + image.width() % 4 * 3 / 4).max(1);
        let height = (image.height() / 4 * 3 + image.height() % 4 * 3 / 4).max(1);
        if width == image.width() && height == image.height() {
            break;
        }
        image = image.resize_exact(width, height, FilterType::Triangle);
        let page = encode_once(&image, encoding)?;
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
                .encode_image(image)
                .map_err(|source| Error::Image {
                    context: "encoding JPEG page".into(),
                    source,
                })?;
            ImageFormat::Jpeg
        }
        ImageEncoding::Png => {
            image
                .write_to(
                    &mut std::io::Cursor::new(&mut data),
                    image::ImageFormat::Png,
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
