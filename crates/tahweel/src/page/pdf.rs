use std::path::Path;

use pdfium_render::prelude::{PdfDocument, PdfRenderConfig, Pdfium, PdfiumError};

use crate::{ColorMode, Error, PageImage, PageOptions, Result};

/// Loads only on the blocking producer, and only when there are pending PDF documents.
pub(crate) fn load_pdfium(explicit: Option<&Path>) -> Result<Pdfium> {
    let mut attempted = Vec::new();
    if let Some(path) = explicit {
        attempted.push(path.display().to_string());
        return initialized(Pdfium::bind_to_library(path)).map_err(|source| {
            Error::PdfiumUnavailable {
                attempted,
                source: source.into(),
            }
        });
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let path = Pdfium::pdfium_platform_library_name_at_path(directory);
        attempted.push(path.display().to_string());
        if let Ok(pdfium) = initialized(Pdfium::bind_to_library(path)) {
            return Ok(pdfium);
        }
    } else {
        attempted.push("beside the executable (executable path unavailable)".into());
    }
    attempted.push(format!(
        "{} (system library search)",
        Pdfium::pdfium_platform_library_name().to_string_lossy()
    ));
    initialized(Pdfium::bind_to_system_library()).map_err(|source| Error::PdfiumUnavailable {
        attempted,
        source: source.into(),
    })
}

fn initialized(
    bindings: std::result::Result<
        Box<dyn pdfium_render::prelude::PdfiumLibraryBindings>,
        PdfiumError,
    >,
) -> std::result::Result<Pdfium, PdfiumError> {
    match bindings {
        Ok(bindings) => Ok(Pdfium::new(bindings)),
        // pdfium-render 0.9 stores its bindings for the lifetime of the process. This
        // branch proves they already exist, so Default reuses them without loading or panicking.
        Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Ok(Pdfium::default()),
        Err(error) => Err(error),
    }
}

pub(crate) fn render_pdf(
    document: &PdfDocument<'_>,
    index: usize,
    options: &PageOptions,
    limit: Option<usize>,
) -> Result<PageImage> {
    let pdf_error = |source| Error::Pdf {
        context: "rendering PDF page".into(),
        source,
    };
    // The producer obtains page counts from PDFium's i32 page-count API.
    let index = i32::try_from(index).expect("page index is below PDFium's page count");
    let page = document.pages().get(index).map_err(pdf_error)?;
    let width = (f64::from(page.width().value) * f64::from(options.dpi) / 72.0).floor();
    let height = (f64::from(page.height().value) * f64::from(options.dpi) / 72.0).floor();
    let (width, height) = dimensions(width, height)?;
    let config = PdfRenderConfig::new()
        .set_target_size(width, height)
        .use_grayscale_rendering(options.color == ColorMode::Grayscale);
    let bitmap = page.render_with_config(&config).map_err(pdf_error)?;
    let image = bitmap.as_image().map_err(pdf_error)?;
    drop(bitmap);
    super::encode(image, options, limit)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "The caller supplies whole pixels; bounds are checked before conversion."
)]
fn dimensions(width: f64, height: f64) -> Result<(i32, i32)> {
    // Apply the same default allocation limit as image inputs, before asking PDFium for a bitmap.
    let limit = image::Limits::default().max_alloc.unwrap_or(u64::MAX);
    if !width.is_finite()
        || !height.is_finite()
        || width < 1.0
        || height < 1.0
        || width > f64::from(i32::MAX)
        || height > f64::from(i32::MAX)
    {
        return Err(Error::PageDimensions { width, height });
    }
    let (w, h) = (width as i32, height as i32);
    if u64::from(w.unsigned_abs()) * u64::from(h.unsigned_abs()) * 4 > limit {
        return Err(Error::PageDimensions { width, height });
    }
    Ok((w, h))
}
