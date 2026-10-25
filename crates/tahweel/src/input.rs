use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// A local file or directory to convert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A PDF, JPEG, PNG, or directory containing those files.
    Path(PathBuf),
}

impl<P: Into<PathBuf>> From<P> for Input {
    fn from(path: P) -> Self {
        Self::Path(path.into())
    }
}

impl Input {
    /// The path supplied by the caller.
    pub fn path(&self) -> &Path {
        let Self::Path(path) = self;
        path
    }

    /// Expands a file or directory into documents, sorted by path.
    ///
    /// Explicit files are checked by their header, directory entries by extension. Explicit
    /// directory symlinks are accepted; symlinks found during traversal are not descended into.
    /// `excluded` skips an output directory nested in the input;
    /// an output directory equal to the input itself is not excluded.
    pub fn discover(&self, recursive: bool, excluded: Option<&Path>) -> Result<Vec<Document>> {
        let path = self.path();
        let metadata = fs::metadata(path)
            .map_err(|source| Error::io(format!("reading {}", path.display()), source))?;
        if metadata.is_file() {
            return Ok(vec![Document::new(path, detect_kind(path)?)]);
        }
        if !metadata.is_dir() {
            return Err(Error::UnsupportedInput { path: path.into() });
        }
        let excluded = excluded.and_then(|path| dunce::canonicalize(path).ok());
        let mut directories = vec![path.to_owned()];
        let mut documents = Vec::new();
        while let Some(directory) = directories.pop() {
            let context = |source| Error::io(format!("reading {}", directory.display()), source);
            for entry in fs::read_dir(&directory).map_err(context)? {
                let entry = entry.map_err(context)?;
                let kind = entry.file_type().map_err(context)?;
                let path = entry.path();
                if recursive && kind.is_dir() {
                    if let Some(excluded) = &excluded
                        && dunce::canonicalize(&path).map_err(context)? == *excluded
                    {
                        continue;
                    }
                    directories.push(path);
                } else if let Some(kind) = extension_kind(&path)
                    && path.is_file()
                {
                    documents.push(Document::new(&path, kind));
                }
            }
        }
        documents.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(documents)
    }
}

/// A discovered document, before its pages are rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Document {
    /// Local input path.
    pub path: PathBuf,
    /// How the input is decoded.
    pub kind: DocumentKind,
    /// Output stem; the pipeline adds a numeric suffix when names collide.
    pub id: String,
    /// File name for display.
    pub title: String,
}

impl Document {
    pub(crate) fn new(path: &Path, kind: DocumentKind) -> Self {
        Self {
            path: path.to_owned(),
            kind,
            id: path
                .file_stem()
                .unwrap_or_else(|| "document".as_ref())
                .to_string_lossy()
                .into(),
            title: path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into(),
        }
    }
}

/// How a document supplies its pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocumentKind {
    /// A PDF; every page is rendered, including pages with embedded text.
    Pdf,
    /// A JPEG or PNG containing one page.
    Image,
}

fn extension_kind(path: &Path) -> Option<DocumentKind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "pdf" => Some(DocumentKind::Pdf),
        "jpg" | "jpeg" | "png" => Some(DocumentKind::Image),
        _ => None,
    }
}

fn detect_kind(path: &Path) -> Result<DocumentKind> {
    let read = || -> std::io::Result<_> {
        let mut header = Vec::with_capacity(8);
        fs::File::open(path)?.take(8).read_to_end(&mut header)?;
        Ok(header)
    };
    let header =
        read().map_err(|source| Error::io(format!("reading {}", path.display()), source))?;
    if header.starts_with(b"%PDF-") {
        Ok(DocumentKind::Pdf)
    } else if header.starts_with(b"\xff\xd8\xff") || header.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok(DocumentKind::Image)
    } else {
        Err(Error::UnsupportedInput { path: path.into() })
    }
}
