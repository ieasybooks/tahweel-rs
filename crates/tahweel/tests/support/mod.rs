use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn pdfium_library() -> Option<PathBuf> {
    let Some(path) = std::env::var_os("TAHWEEL_TEST_PDFIUM") else {
        eprintln!("skipping PDF test: set TAHWEEL_TEST_PDFIUM to a compatible PDFium library");
        return None;
    };
    Some(path.into())
}

// Minimal PDFs with an embedded text layer and distinct page widths, generated locally.
// The fake recognizer identifies rendered pages by pixel width at 72 DPI.
pub(crate) fn pdf(path: &Path, widths: &[u32]) {
    use std::fmt::Write;

    let font = 3 + widths.len() * 2;
    let kids = (0..widths.len())
        .map(|page| format!("{} 0 R", 3 + page * 2))
        .collect::<Vec<_>>()
        .join(" ");
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Count {} /Kids [{kids}] >>", widths.len()),
    ];
    for (index, width) in widths.iter().enumerate() {
        let content = format!("BT /F1 6 Tf 2 2 Td (page {}) Tj ET", index + 1);
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} 20] /Resources << /Font << /F1 {font} 0 R >> >> /Contents {} 0 R >>", 4 + index * 2));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    let mut content = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(content.len());
        writeln!(content, "{} 0 obj\n{object}\nendobj", index + 1).unwrap();
    }
    let xref = content.len();
    writeln!(
        content,
        "xref\n0 {}\n0000000000 65535 f ",
        objects.len() + 1
    )
    .unwrap();
    for offset in offsets {
        writeln!(content, "{offset:010} 00000 n ").unwrap();
    }
    writeln!(
        content,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF",
        objects.len() + 1
    )
    .unwrap();
    fs::write(path, content).unwrap();
}
