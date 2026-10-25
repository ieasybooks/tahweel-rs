//! Command-line arguments. Doc comments here are the `--help` text.
#![allow(
    clippy::doc_markdown,
    reason = "doc comments are --help text, where backticks would show literally"
)]

use std::path::PathBuf;

use tahweel::Format;
use usage_rs::ValueEnum;

const HELP_FOOTER: &str = "\
PDF inputs require PDFium, included in release archives. Image inputs need no PDFium.
Completed pages are saved under tahweel/checkpoints in the user cache directory for resuming.
Ctrl-C stops new pages and lets running pages finish cleanup; a second Ctrl-C exits immediately.

Use @FILE to read options and inputs from a file of shell-style words; @@ escapes a literal @.
Boolean conversion flags accept a --no- prefix, such as --no-recursive, to override presets.";

/// Convert PDFs and images into TXT, DOCX and JSON with Google Drive OCR.
///
/// Conversion is the default command. Use completions to set up your shell, or
/// spec to export the CLI definition for documentation and other Usage tools.
#[derive(Debug, Clone, usage_rs::Cli)]
#[usage(
    bin = "tahweel",
    version,
    repository = env!("CARGO_PKG_REPOSITORY"),
    license = env!("CARGO_PKG_LICENSE"),
    completion,
    default_subcommand = "convert",
    default_subcommand_flags,
    default_subcommand_help,
    arg_required_else_help,
    unknown_flags = "error",
    max_term_width = 100,
    exit_code(0, "Success"),
    exit_code(1, "Setup, filesystem or other runtime error"),
    exit_code(2, "Invalid arguments"),
    example("tahweel book.pdf --recognizer drive-oauth", help = "Convert a book with Google sign-in"),
    example("tahweel completions zsh --install", help = "Install shell completions")
)]
pub(crate) struct Cli {
    #[usage(subcommand)]
    pub(crate) command: Command,
}

/// Operations available from the command line.
#[derive(Debug, Clone, usage_rs::Subcommands)]
pub(crate) enum Command {
    /// Convert PDF, JPEG and PNG files or folders.
    Convert(Box<Args>),
    /// Generate or install shell completions.
    Completions(Completions),
    /// Print the Usage specification as KDL for docs, man pages and tooling.
    Spec,
}

/// Generate a completion script, or install it in the shell's user directory.
///
/// Scripts call tahweel directly; no separate usage executable is needed.
/// Installation prints any shell setup still required and preserves unrelated files.
#[derive(Debug, Clone, usage_rs::Args)]
#[usage(
    unknown_flags = "error",
    example("tahweel completions bash", help = "Print a script to stdout"),
    example(
        "tahweel completions zsh --install --dry-run",
        help = "Preview installation"
    )
)]
pub(crate) struct Completions {
    /// Shell to generate completions for.
    #[usage(arg, choices("bash", "elvish", "fish", "nu", "powershell", "zsh"))]
    pub(crate) shell: String,

    /// Install into this shell's user completion directory.
    #[usage(long)]
    pub(crate) install: bool,

    /// Show the destination and setup instructions without writing a file.
    #[usage(long, requires("--install"))]
    pub(crate) dry_run: bool,
}

/// Convert PDFs and images into TXT, DOCX and JSON with Google Drive OCR.
///
/// Inputs can be PDF, JPEG and PNG files, or folders containing them.
/// Every page is recognized, including PDFs that already have a text layer.
/// Pages use grayscale PNG. Image inputs are capped at a 2,339-pixel long edge without
/// enlargement. Pages over 2,000,000 bytes fall back to JPEG, then shrink to fit.
#[derive(Debug, Clone, usage_rs::Args)]
#[usage(
    unknown_flags = "error",
    max_term_width = 100,
    after_help = HELP_FOOTER,
    exit_code(3, "Finished, but some inputs or documents failed"),
    exit_code(130, "Interrupted with Ctrl-C"),
    example("tahweel convert books/ --recognizer drive-oauth -r -o text", help = "Mirror a folder of books"),
    example("tahweel convert book.pdf --recognizer drive-service-account --service-account key.json -f txt,json", help = "Convert with a service account"),
    example("tahweel convert book.pdf --recognizer drive-oauth --skip-existing", help = "Skip a book whose outputs already exist")
)]
pub(crate) struct Args {
    /// PDF, JPEG and PNG files or folders to convert.
    #[usage(arg, required, name = "INPUT", value_hint = usage_rs::ValueHint::AnyPath)]
    pub(crate) inputs: Vec<PathBuf>,

    /// Recognizer to use for the entire run. Required; no automatic switching.
    #[usage(long, value_enum, value_name = "BACKEND")]
    pub(crate) recognizer: Recognizer,

    /// OCR language code, such as ar or en.
    #[usage(short = 'l', long, value_name = "LANG", default = "ar")]
    pub(crate) language: String,

    /// Include files in subdirectories. Directory symlinks are not followed.
    #[usage(short = 'r', long, negate = "no-recursive")]
    pub(crate) recursive: bool,

    /// Hide progress bars.
    #[usage(short, long, negate = "no-quiet")]
    pub(crate) quiet: bool,

    /// More logging (-v debug, -vv trace). RUST_LOG overrides.
    #[usage(short, long, count)]
    pub(crate) verbose: u8,

    #[usage(flatten, next_help_heading = "Output")]
    pub(crate) output: Output,

    #[usage(flatten, next_help_heading = "Pages")]
    pub(crate) pages: Pages,

    #[usage(flatten, next_help_heading = "Google Drive")]
    pub(crate) drive: Drive,
}

/// Explicit OCR backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Recognizer {
    /// Sign in to Google in the browser; reuse the saved sign-in on later runs.
    DriveOauth,
    /// Use one or more service-account JSON keys.
    DriveServiceAccount,
}

impl Recognizer {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::DriveOauth => "drive-oauth",
            Self::DriveServiceAccount => "drive-service-account",
        }
    }
}

/// Where outputs go and what they contain.
#[derive(Debug, Clone, usage_rs::Args)]
pub(crate) struct Output {
    /// Directory for outputs; selects tree-to-tree mode unless --output-mode is given.
    #[usage(short = 'o', long = "output-dir", value_name = "DIR", value_hint = usage_rs::ValueHint::DirPath)]
    pub(crate) dir: Option<PathBuf>,

    /// Destination layout [default: side-by-side without -o, tree-to-tree with -o].
    #[usage(long = "output-mode", value_enum, value_name = "MODE")]
    pub(crate) mode: Option<OutputMode>,

    /// Output formats, comma-separated: txt, docx, json or all.
    #[usage(
        short = 'f',
        long = "format",
        value_name = "FORMATS",
        delimiter = ',',
        default = "all",
        choices("all", "txt", "docx", "json")
    )]
    pub(crate) formats: Vec<FormatChoice>,

    /// Text between pages in TXT output. Decodes \n (newline), \t (tab) and \\ (backslash).
    #[usage(long, value_name = "TEXT", default = r"\n\nPAGE_SEPARATOR\n\n")]
    pub(crate) page_separator: String,

    /// Skip documents when every requested output file already exists.
    #[usage(long, negate = "no-skip-existing")]
    pub(crate) skip_existing: bool,
}

/// Value of --output-mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputMode {
    /// Write beside each input file; cannot be used with -o.
    SideBySide,
    /// Mirror input folders in -o, keeping multiple roots separate.
    TreeToTree,
    /// Put all outputs directly in -o.
    Flat,
}

/// A value of `--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormatChoice {
    All,
    One(Format),
}

impl std::str::FromStr for FormatChoice {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "txt" => Ok(Self::One(Format::Txt)),
            "docx" => Ok(Self::One(Format::Docx)),
            "json" => Ok(Self::One(Format::Json)),
            _ => Err("expected txt, docx, json or all".into()),
        }
    }
}

/// Page rendering and its runtime library.
#[derive(Debug, Clone, usage_rs::Args)]
pub(crate) struct Pages {
    /// PDF rendering resolution in dots per inch. Must be greater than zero.
    #[usage(long, value_name = "N", default = "200")]
    pub(crate) dpi: u32,

    /// PDFium shared library file. By default, look beside tahweel, then on the system library
    /// search path. Needed only for PDF inputs; included in release archives.
    #[usage(long, value_name = "PATH", env = "TAHWEEL_PDFIUM", value_hint = usage_rs::ValueHint::FilePath)]
    pub(crate) pdfium: Option<PathBuf>,
}

/// Google Drive credentials and request settings.
#[derive(Debug, Clone, usage_rs::Args)]
pub(crate) struct Drive {
    /// drive-service-account: service-account JSON keys (repeat or comma-separate).
    /// At least one is required.
    #[usage(long = "service-account", value_name = "FILE", env = "TAHWEEL_SERVICE_ACCOUNTS", hide_env_values, delimiter = ',', value_hint = usage_rs::ValueHint::FilePath)]
    pub(crate) service_accounts: Vec<PathBuf>,

    /// drive-oauth: saved sign-in file [default: tahweel/google-token.json in the local data
    /// directory, such as ~/.local/share/tahweel/google-token.json on Linux].
    /// A missing file starts browser sign-in. Delete this file to sign out.
    #[usage(long = "drive-token", value_name = "FILE", value_hint = usage_rs::ValueHint::FilePath)]
    pub(crate) token: Option<PathBuf>,

    /// Both Drive recognizers: maximum concurrent pages per account, including cleanup.
    #[usage(long = "drive-concurrency", value_name = "N", default = "8")]
    pub(crate) concurrency: usize,

    /// Both Drive recognizers: attempts per HTTP operation, including the first.
    #[usage(long = "drive-attempts", value_name = "N", default = "5")]
    pub(crate) attempts: u32,

    /// Test-only origin for local mock API, upload, token and sign-in endpoints.
    #[usage(long = "drive-test-url", value_name = "URL", hide)]
    pub(crate) test_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use usage_rs::test::{self as harness, Files};

    use super::{Cli, Command};

    #[test]
    fn conversion_defaults_match_the_selected_settings() {
        let words =
            ["tahweel", "page.png", "--recognizer", "drive-oauth"].map(std::ffi::OsStr::new);
        let cli = Cli::parse_from(&words).unwrap();
        let Command::Convert(args) = cli.command else {
            panic!("expected conversion");
        };
        assert_eq!(args.language, "ar");
        assert_eq!(args.pages.dpi, 200);
        assert_eq!(args.drive.concurrency, 8);
        assert_eq!(args.drive.attempts, 5);
    }

    #[test]
    fn completions_offer_recognizers_formats_and_path_kinds() {
        assert_eq!(
            harness::candidates(Cli::spec(), "tahweel --recognizer drive-o"),
            ["drive-oauth"]
        );
        assert_eq!(
            harness::candidates(Cli::spec(), "tahweel convert --format d"),
            ["docx"]
        );
        assert_eq!(
            harness::completion(Cli::spec(), "tahweel convert --output-dir ").files,
            Some(Files::Dirs)
        );
        for prefix in [
            "tahweel convert ",
            "tahweel convert --service-account ",
            "tahweel convert --pdfium ",
            "tahweel convert --drive-token ",
        ] {
            assert_eq!(
                harness::completion(Cli::spec(), prefix).files,
                Some(Files::Any)
            );
        }
    }
}
