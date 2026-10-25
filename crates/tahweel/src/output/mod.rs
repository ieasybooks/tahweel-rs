mod docx;
mod layout;

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

pub use layout::OutputMode;

use crate::{Error, Result};

/// An output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Format {
    /// Plain UTF-8 text with a separator between pages.
    Txt,
    /// A Word document with a paragraph for each page.
    Docx,
    /// A JSON array of objects with `page` and `content` fields.
    Json,
}

impl Format {
    /// All supported formats, in default output order.
    pub const ALL: [Self; 3] = [Self::Txt, Self::Docx, Self::Json];

    /// The lowercase file extension, without a dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Txt => "txt",
            Self::Docx => "docx",
            Self::Json => "json",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.extension())
    }
}

/// Renders recognized pages and writes each output file atomically.
#[derive(Debug, Clone)]
pub struct OutputWriter {
    formats: Vec<Format>,
    page_separator: String,
}

impl Default for OutputWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputWriter {
    /// Writes TXT, DOCX and JSON, with `\n\nPAGE_SEPARATOR\n\n` between TXT pages.
    pub fn new() -> Self {
        Self {
            formats: Format::ALL.into(),
            page_separator: "\n\nPAGE_SEPARATOR\n\n".into(),
        }
    }

    /// Replaces the formats to write. Duplicates are removed, keeping the first occurrence.
    #[must_use]
    pub fn formats(mut self, formats: impl IntoIterator<Item = Format>) -> Self {
        self.formats.clear();
        for format in formats {
            if !self.formats.contains(&format) {
                self.formats.push(format);
            }
        }
        self
    }

    /// Changes the text between TXT pages. Line endings are normalized to LF.
    #[must_use]
    pub fn page_separator(mut self, separator: impl Into<String>) -> Self {
        self.page_separator = normalize_newlines(&separator.into());
        self
    }

    /// Renders one format in memory. Page numbers in JSON start at 1.
    pub fn render(&self, format: Format, pages: &[String]) -> Result<Vec<u8>> {
        match format {
            Format::Txt => Ok(clean_pages(pages).join(&self.page_separator).into_bytes()),
            Format::Json => {
                #[derive(serde::Serialize)]
                struct Page<'a> {
                    page: usize,
                    content: &'a str,
                }
                let pages = clean_pages(pages);
                let pages: Vec<_> = pages
                    .iter()
                    .enumerate()
                    .map(|(index, text)| Page {
                        page: index + 1,
                        content: text,
                    })
                    .collect();
                serde_json::to_vec_pretty(&pages).map_err(|source| Error::Output {
                    format,
                    source: source.into(),
                })
            }
            Format::Docx => docx::render(pages).map_err(|source| Error::Output {
                format,
                source: source.into(),
            }),
        }
    }

    /// Every destination for `name`, in format order.
    pub fn outputs(&self, dir: &Path, name: &str) -> Vec<PathBuf> {
        self.formats
            .iter()
            .map(|format| dir.join(format!("{name}.{format}")))
            .collect()
    }

    /// True when at least one format was requested and every destination is a file.
    pub fn exists(&self, dir: &Path, name: &str) -> bool {
        let outputs = self.outputs(dir, name);
        !outputs.is_empty() && outputs.iter().all(|path| path.is_file())
    }

    /// Writes every requested format, replacing each destination by an atomic rename.
    ///
    /// The temporary files live in `dir` and are removed if rendering or writing fails.
    /// `name` is an output file stem, as supplied by a discovered [`crate::Document`].
    /// An empty format list is useful when only the pipeline's in-memory results are needed.
    pub fn write(&self, dir: &Path, name: &str, pages: &[String]) -> Result<Vec<PathBuf>> {
        if self.formats.is_empty() {
            return Ok(Vec::new());
        }
        std::fs::create_dir_all(dir)
            .map_err(|source| Error::io(format!("creating {}", dir.display()), source))?;
        let paths = self.outputs(dir, name);
        let mut staged = Vec::new();
        for (format, path) in self.formats.iter().zip(&paths) {
            let bytes = self.render(*format, pages)?;
            let stage = || -> std::io::Result<_> {
                let mut temporary = output_file(dir)?;
                temporary.write_all(&bytes)?;
                temporary.as_file().sync_all()?;
                Ok(temporary)
            };
            staged.push(
                stage()
                    .map_err(|source| Error::io(format!("writing {}", path.display()), source))?,
            );
        }
        for (temporary, path) in staged.into_iter().zip(&paths) {
            temporary
                .persist(path)
                .map_err(|error| Error::io(format!("writing {}", path.display()), error.error))?;
        }
        Ok(paths)
    }
}

fn clean_pages(pages: &[String]) -> Vec<String> {
    pages
        .iter()
        .map(|page| normalize_newlines(page.trim()))
        .collect()
}

fn output_file(dir: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Let the OS apply the process umask, as it does for File::create.
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o666))
            .tempfile_in(dir)
    }
    #[cfg(not(unix))]
    tempfile::NamedTempFile::new_in(dir)
}

pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
