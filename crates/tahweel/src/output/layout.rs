use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::OutputWriter;
use crate::{Document, Input};
use crate::{Error, Result};

/// Where a run puts document outputs. Defaults to side-by-side output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OutputMode {
    /// Beside each input file.
    #[default]
    SideBySide,
    /// Mirror input directories under this directory, with all formats together. Several
    /// directory inputs are kept apart by name; explicit files go directly in it.
    TreeToTree(PathBuf),
    /// Put every output directly in this directory.
    Flat(PathBuf),
}

impl OutputMode {
    /// The output directory; `None` in side-by-side mode.
    pub fn dir(&self) -> Option<&Path> {
        match self {
            Self::SideBySide => None,
            Self::TreeToTree(dir) | Self::Flat(dir) => Some(dir),
        }
    }
}

/// One document's output directory and reserved name.
pub(crate) struct Target {
    pub(crate) dir: PathBuf,
    pub(crate) name: String,
}

impl Target {
    pub(crate) fn exists(&self, writer: &OutputWriter) -> bool {
        writer.exists(&self.dir, &self.name)
    }
}

/// Run-wide layout and reservations for output files and their parent directories.
pub(crate) struct Layout {
    mode: OutputMode,
    writer: OutputWriter,
    roots: Vec<Option<(PathBuf, PathBuf)>>,
    directories: HashSet<String>,
    reserved: HashSet<String>,
    names: HashSet<String>,
}

impl Layout {
    pub(crate) fn new(mode: OutputMode, writer: OutputWriter, inputs: &[Input]) -> Self {
        let separate_roots = inputs
            .iter()
            .filter(|input| matches!(input, Input::Path(path) if path.is_dir()))
            .count()
            > 1;
        let mut names = HashSet::new();
        // Allocate roots before discovery: a failed input still owns its directory name.
        let roots: Vec<_> = inputs
            .iter()
            .map(|input| {
                let OutputMode::TreeToTree(output_dir) = &mode else {
                    return None;
                };
                let Input::Path(path) = input;
                if !path.is_dir() {
                    return None;
                }
                let mut destination = output_dir.clone();
                if separate_roots {
                    let root = dunce::canonicalize(path).unwrap_or_else(|_| path.clone());
                    let name = root
                        .file_name()
                        .unwrap_or_else(|| "root".as_ref())
                        .to_string_lossy();
                    destination.push(unique_name(&name, |name| names.insert(name.to_lowercase())));
                }
                Some((path.clone(), destination))
            })
            .collect();
        let mut directories = HashSet::new();
        for (_, destination) in roots.iter().flatten() {
            // Even empty or undiscoverable roots keep their directory paths. Unusable
            // destinations are diagnosed later, for each affected document.
            if let Ok(dir) = normalize_dir(destination) {
                directories.extend(dir.ancestors().map(path_key));
            }
        }
        Self {
            mode,
            writer,
            roots,
            directories,
            reserved: HashSet::new(),
            names: HashSet::new(),
        }
    }

    /// The output directory for a document discovered from this input.
    pub(crate) fn directory(&self, document: &Document, input: usize) -> PathBuf {
        match &self.mode {
            OutputMode::SideBySide => parent(&document.path).to_owned(),
            OutputMode::TreeToTree(output_dir) => match &self.roots[input] {
                Some((source, destination)) => {
                    // Discovery preserves the input path prefix.
                    let relative = document
                        .path
                        .strip_prefix(source)
                        .expect("discovered file is inside its input root");
                    destination.join(parent(relative))
                }
                None => output_dir.clone(),
            },
            OutputMode::Flat(output_dir) => output_dir.clone(),
        }
    }

    /// Explicit inputs can use an output extension, so reserve their paths too.
    pub(crate) fn reserve_input(&mut self, path: &Path) -> Result<()> {
        let canonical = dunce::canonicalize(path)
            .map_err(|source| Error::io(format!("resolving {}", path.display()), source))?;
        let named = normalize_dir(parent(path))?.join(path.file_name().unwrap_or(path.as_os_str()));
        self.reserved.insert(path_key(&canonical));
        self.reserved.insert(path_key(&named));
        Ok(())
    }

    /// Register all known directories before any document claims a file name.
    pub(crate) fn require_directory(&mut self, dir: &Path) -> Result<PathBuf> {
        let dir = normalize_dir(dir)?;
        self.directories.extend(dir.ancestors().map(path_key));
        Ok(dir)
    }

    pub(crate) fn plan(&mut self, document: &Document, dir: &Path) -> Result<Target> {
        self.reserve(dir, &document.id)
    }

    /// Finds a name whose files are all free in `dir`, then reserves those files.
    fn reserve(&mut self, dir: &Path, name: &str) -> Result<Target> {
        let dir = normalize_dir(dir)?;
        let writer = &self.writer;
        let files = |name: &str| writer.outputs(&dir, name);
        let name = unique_name(name, |candidate| {
            !self.names.contains(&path_key(&dir.join(candidate)))
                && files(candidate).iter().all(|path| self.is_free(path))
        });
        self.reserved
            .extend(files(&name).iter().map(|path| path_key(path)));
        self.names.insert(path_key(&dir.join(&name)));
        Ok(Target { dir, name })
    }

    /// Whether nothing in this run and no existing directory uses `path`.
    fn is_free(&self, path: &Path) -> bool {
        let key = path_key(path);
        !self.reserved.contains(&key) && !self.directories.contains(&key) && !path.is_dir()
    }
}

/// Returns `name` when `claim` accepts it, otherwise the first of `name-2`, `name-3`, …
/// that it accepts.
pub(crate) fn unique_name(name: &str, mut claim: impl FnMut(&str) -> bool) -> String {
    let mut candidate = name.to_owned();
    let mut suffix = 2;
    while !claim(&candidate) {
        candidate = format!("{name}-{suffix}");
        suffix += 1;
    }
    candidate
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Resolve existing symlinks and normalize relative paths, including directories not created yet.
fn normalize_dir(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)
        .map_err(|err| Error::io(format!("resolving {}", path.display()), err))?;
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
                continue;
            }
            Component::CurDir => continue,
            // A drive or root alone needs no resolving; on Windows, canonicalizing `\\?\C:`
            // fails and `C:` would name the drive's current directory.
            Component::Prefix(_) | Component::RootDir => {
                normalized.push(component);
                continue;
            }
            Component::Normal(_) => normalized.push(component),
        }
        match dunce::canonicalize(&normalized) {
            Ok(path) => {
                if !path.is_dir() {
                    return Err(Error::io(
                        format!("resolving {}", path.display()),
                        std::io::ErrorKind::NotADirectory.into(),
                    ));
                }
                normalized = path;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(Error::io(
                    format!("resolving {}", normalized.display()),
                    err,
                ));
            }
        }
    }
    Ok(normalized)
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentKind, Format};

    #[test]
    fn reserves_case_insensitive_names_and_all_directories_before_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let writer = OutputWriter::new().formats([Format::Txt]);
        let mut layout = Layout::new(OutputMode::Flat(dir.into()), writer.clone(), &[]);
        let root = layout.require_directory(dir).unwrap();
        layout.require_directory(&dir.join("book-2.txt")).unwrap();
        let document = Document::new(Path::new("book.pdf"), DocumentKind::Pdf);
        let first = layout.plan(&document, &root).unwrap();
        assert_eq!(first.name, "book");
        let second = layout
            .plan(
                &Document::new(Path::new("BOOK.jpg"), DocumentKind::Image),
                &root,
            )
            .unwrap();
        assert_eq!(second.name, "BOOK-3");
        assert_eq!(layout.plan(&document, &root).unwrap().name, "book-4");
        assert!(!first.exists(&writer));
        writer
            .write(&first.dir, &first.name, &["text".into()])
            .unwrap();
        assert!(first.exists(&writer));
        // Existing files retain their names, so skip_existing can find them on the next run.
        let mut next = Layout::new(OutputMode::Flat(dir.into()), writer, &[]);
        assert_eq!(next.plan(&document, &root).unwrap().name, "book");
    }

    #[test]
    fn explicit_inputs_with_output_extensions_are_not_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("book.txt");
        std::fs::write(&input, b"%PDF-1.7").unwrap();
        let mut layout = Layout::new(OutputMode::SideBySide, OutputWriter::new(), &[]);
        layout.reserve_input(&input).unwrap();
        let document = Document::new(&input, DocumentKind::Pdf);
        assert_eq!(layout.plan(&document, temp.path()).unwrap().name, "book-2");
    }

    #[test]
    fn names_are_distinct_when_only_in_memory_results_are_requested() {
        let temp = tempfile::tempdir().unwrap();
        let mut layout = Layout::new(OutputMode::SideBySide, OutputWriter::new().formats([]), &[]);
        let document = Document::new(Path::new("book.pdf"), DocumentKind::Pdf);
        assert_eq!(layout.plan(&document, temp.path()).unwrap().name, "book");
        assert_eq!(layout.plan(&document, temp.path()).unwrap().name, "book-2");
    }

    #[test]
    fn tree_to_tree_mirrors_one_root_and_separates_multiple_roots() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("a/books");
        let second = temp.path().join("b/books");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let out = temp.path().join("out");
        let document = Document::new(&first.join("nested/book.pdf"), DocumentKind::Pdf);
        let single = Layout::new(
            OutputMode::TreeToTree(out.clone()),
            OutputWriter::new(),
            &[Input::from(&first)],
        );
        assert_eq!(single.directory(&document, 0), out.join("nested"));
        let multiple = Layout::new(
            OutputMode::TreeToTree(out.clone()),
            OutputWriter::new(),
            &[Input::from(&first), Input::from(&second)],
        );
        assert_eq!(multiple.directory(&document, 0), out.join("books/nested"));
        let document = Document::new(&second.join("book.pdf"), DocumentKind::Pdf);
        assert_eq!(multiple.directory(&document, 1), out.join("books-2"));
        let adjacent = Layout::new(OutputMode::SideBySide, OutputWriter::new(), &[]);
        assert_eq!(adjacent.directory(&document, 0), second);
    }

    #[cfg(unix)]
    #[test]
    fn input_file_symlinks_are_reserved_as_well_as_their_targets() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original.pdf");
        let alias = temp.path().join("book.txt");
        std::fs::write(&original, b"%PDF-1.7").unwrap();
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let mut layout = Layout::new(OutputMode::SideBySide, OutputWriter::new(), &[]);
        layout.reserve_input(&alias).unwrap();
        let document = Document::new(&alias, DocumentKind::Pdf);
        assert_eq!(layout.plan(&document, temp.path()).unwrap().name, "book-2");
    }

    #[cfg(unix)]
    #[test]
    fn directory_aliases_share_reservations() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("out");
        std::fs::create_dir(&dir).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&dir, &alias).unwrap();
        let mut layout = Layout::new(OutputMode::Flat(dir.clone()), OutputWriter::new(), &[]);
        let document = Document::new(Path::new("book.pdf"), DocumentKind::Pdf);
        assert_eq!(layout.plan(&document, &dir).unwrap().name, "book");
        assert_eq!(
            layout
                .plan(&document, &alias.join("nested/.."))
                .unwrap()
                .name,
            "book-2"
        );
    }
}
