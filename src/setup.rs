//! Validate arguments into a run plan before loading credentials, `PDFium` or inputs.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use tahweel::{Format, Input, OutputMode, OutputWriter, PageOptions};
use tahweel_drive::oauth::{self, SignIn};
use tahweel_drive::{
    DriveAuth, DriveConfig, DriveError, DriveRecognizer, OAuthConfig, OAuthToken, ServiceAccountKey,
};
use usage_rs::{Event, Parser, ValidationError};

use crate::cli::{self, Args, Cli, Command, Completions, FormatChoice, Recognizer};
use crate::ui::Ui;

/// An operation with its command-line options checked.
pub(crate) enum Plan {
    Convert(Box<Run>),
    Completions(Completions),
    Spec,
}

/// Library settings for one conversion run.
pub(crate) struct Run {
    pub(crate) inputs: Vec<Input>,
    pub(crate) drive: Drive,
    pub(crate) output: OutputWriter,
    pub(crate) output_mode: OutputMode,
    pub(crate) pages: PageOptions,
    pub(crate) pdfium: Option<PathBuf>,
    pub(crate) checkpoint_label: String,
    pub(crate) recursive: bool,
    pub(crate) skip_existing: bool,
    pub(crate) quiet: bool,
    pub(crate) verbose: u8,
}

/// Credential paths are loaded only after the entire plan has been validated.
enum Credentials {
    ServiceAccounts(Vec<PathBuf>),
    OAuth(Option<PathBuf>),
}

/// Checked Drive settings, before credentials or network requests are loaded.
pub(crate) struct Drive {
    credentials: Credentials,
    language: Option<String>,
    concurrency: usize,
    attempts: u32,
    test_url: Option<String>,
}

impl Plan {
    pub(crate) fn parse_from<'v>(
        words: &[&'v OsStr],
    ) -> Result<Self, usage_rs::Error<'static, 'v>> {
        let cli = Cli::parse_from(words)?;
        let typed = typed_options(words)?;
        Ok(match cli.command {
            Command::Convert(args) => Self::Convert(Box::new(
                Run::new(&args, &typed).map_err(ValidationError::into_parse_error)?,
            )),
            Command::Completions(args) => Self::Completions(args),
            Command::Spec => Self::Spec,
        })
    }
}

// Binding events contain only explicit argv flags. Environment defaults do not count as
// supplied flags; an exported service-account list is ignored by drive-oauth.
fn typed_options<'v>(
    argv: &[&'v OsStr],
) -> Result<HashSet<&'static str>, usage_rs::Error<'static, 'v>> {
    let mut typed = HashSet::new();
    let mut parser = Parser::new(Cli::command(), argv);
    while let Some(event) = parser.next_event() {
        if let Event::Flag { flag, .. } = event? {
            typed.insert(flag.name);
        }
    }
    Ok(typed)
}

impl Run {
    fn new(args: &Args, typed: &HashSet<&str>) -> Result<Self, ValidationError> {
        let unused = match args.recognizer {
            Recognizer::DriveOauth => "--service-account",
            Recognizer::DriveServiceAccount => "--drive-token",
        };
        if typed.contains(&unused[2..]) {
            return Err(ValidationError::field(unused).reason(format!(
                "isn't used by --recognizer {}",
                args.recognizer.name()
            )));
        }
        for input in &args.inputs {
            nonempty_path("INPUT", input)?;
        }
        if args.language.trim().is_empty() {
            return Err(ValidationError::field("--language").reason("must not be empty"));
        }
        if args.pages.dpi == 0 {
            return Err(ValidationError::field("--dpi").reason("must be greater than zero"));
        }
        if let Some(path) = &args.pages.pdfium {
            nonempty_path("--pdfium", path)?;
        }
        if args.drive.attempts == 0 {
            return Err(ValidationError::field("--drive-attempts").reason("must be at least 1"));
        }
        let credentials = match args.recognizer {
            Recognizer::DriveServiceAccount => {
                if args.drive.service_accounts.is_empty() {
                    return Err(ValidationError::field("--service-account").reason(
                        "at least one key is required with --recognizer drive-service-account; \
                         use --service-account or TAHWEEL_SERVICE_ACCOUNTS",
                    ));
                }
                for path in &args.drive.service_accounts {
                    nonempty_path("--service-account", path)?;
                }
                Credentials::ServiceAccounts(args.drive.service_accounts.clone())
            }
            Recognizer::DriveOauth => {
                if let Some(path) = &args.drive.token {
                    nonempty_path("--drive-token", path)?;
                }
                Credentials::OAuth(args.drive.token.clone())
            }
        };
        let accounts = match &credentials {
            Credentials::ServiceAccounts(paths) => paths.len(),
            Credentials::OAuth(_) => 1,
        };
        if !accounts
            .checked_mul(args.drive.concurrency)
            .is_some_and(|total| total > 0 && total <= tokio::sync::Semaphore::MAX_PERMITS / 2)
        {
            return Err(ValidationError::field("--drive-concurrency")
                .reason("must be at least 1 and fit the total page concurrency limit"));
        }
        let test_url = args
            .drive
            .test_url
            .as_deref()
            .map(test_endpoint)
            .transpose()?;
        let mut pages = PageOptions::default();
        pages.dpi = args.pages.dpi;
        Ok(Self {
            inputs: args.inputs.iter().map(Input::from).collect(),
            drive: Drive {
                credentials,
                language: (args.language != "auto").then(|| args.language.clone()),
                concurrency: args.drive.concurrency,
                attempts: args.drive.attempts,
                test_url,
            },
            output: args.output.writer(),
            output_mode: args.output.mode()?,
            pages,
            pdfium: args.pages.pdfium.clone(),
            checkpoint_label: format!("{}:{}", args.recognizer.name(), args.language),
            recursive: args.recursive,
            skip_existing: args.output.skip_existing,
            quiet: args.quiet,
            verbose: args.verbose,
        })
    }
}

fn nonempty_path(flag: &'static str, path: &Path) -> Result<(), ValidationError> {
    if path.as_os_str().is_empty() {
        Err(ValidationError::field(flag).reason("path must not be empty"))
    } else {
        Ok(())
    }
}

fn test_endpoint(value: &str) -> Result<String, ValidationError> {
    let valid = reqwest::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    });
    if !valid {
        return Err(ValidationError::field("--drive-test-url")
            .reason("expected an HTTP(S) base URL without credentials, query or fragment"));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

impl cli::Output {
    fn mode(&self) -> Result<OutputMode, ValidationError> {
        if let Some(dir) = &self.dir {
            nonempty_path("--output-dir", dir)?;
        }
        match (self.mode, self.dir.clone()) {
            (Some(cli::OutputMode::SideBySide) | None, None) => Ok(OutputMode::SideBySide),
            (Some(cli::OutputMode::TreeToTree) | None, Some(dir)) => {
                Ok(OutputMode::TreeToTree(dir))
            }
            (Some(cli::OutputMode::Flat), Some(dir)) => Ok(OutputMode::Flat(dir)),
            (Some(cli::OutputMode::SideBySide), Some(_)) => {
                Err(ValidationError::field("--output-mode")
                    .reason("side-by-side cannot be used with --output-dir"))
            }
            (Some(cli::OutputMode::TreeToTree | cli::OutputMode::Flat), None) => {
                Err(ValidationError::field("--output-mode")
                    .reason("tree-to-tree and flat require --output-dir"))
            }
        }
    }

    fn writer(&self) -> OutputWriter {
        let formats = self.formats.iter().flat_map(|choice| match choice {
            FormatChoice::All => Format::ALL.to_vec(),
            FormatChoice::One(format) => vec![*format],
        });
        OutputWriter::new()
            .formats(formats)
            .page_separator(decode_separator(&self.page_separator))
    }
}

// Decode once, so \\n stays a literal backslash followed by n. Other escapes remain literal.
fn decode_separator(value: &str) -> String {
    let mut decoded = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => decoded.push('\n'),
            Some('t') => decoded.push('\t'),
            Some('\\') | None => decoded.push('\\'),
            Some(other) => {
                decoded.push('\\');
                decoded.push(other);
            }
        }
    }
    decoded
}

impl Drive {
    /// Load credentials and get the first token before any input is rendered.
    pub(crate) async fn recognizer(&self, ui: &Ui) -> anyhow::Result<DriveRecognizer> {
        match &self.credentials {
            Credentials::ServiceAccounts(paths) => {
                let keys = paths
                    .iter()
                    .map(ServiceAccountKey::from_file)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(DriveRecognizer::new(self.config(DriveAuth::ServiceAccounts(keys))).await?)
            }
            Credentials::OAuth(path) => {
                let path = match path {
                    Some(path) => path.clone(),
                    None => oauth::default_token_path()?,
                };
                let (token, saved) = match OAuthToken::load(&path) {
                    Ok(token) => (token, true),
                    Err(DriveError::Io { source, .. })
                        if source.kind() == std::io::ErrorKind::NotFound =>
                    {
                        (self.sign_in(&path, ui).await?, false)
                    }
                    Err(error) => return Err(error.into()),
                };
                match DriveRecognizer::new(self.config(DriveAuth::OAuth(token))).await {
                    Ok(recognizer) => Ok(recognizer),
                    Err(DriveError::Authentication(_) | DriveError::CredentialsRejected)
                        if saved =>
                    {
                        ui.line("Google rejected the saved sign-in; sign in again.");
                        let token = self.sign_in(&path, ui).await?;
                        Ok(DriveRecognizer::new(self.config(DriveAuth::OAuth(token))).await?)
                    }
                    Err(error) => Err(error.into()),
                }
            }
        }
    }

    fn config(&self, auth: DriveAuth) -> DriveConfig {
        let mut config = DriveConfig::new(auth);
        config.language.clone_from(&self.language);
        config.concurrency_per_account = self.concurrency;
        config.max_attempts = self.attempts;
        if let Some(base) = &self.test_url {
            config.api_base.clone_from(base);
            config.upload_base = format!("{base}/upload");
            config.token_url = format!("{base}/token");
        }
        config
    }

    async fn sign_in(&self, path: &Path, ui: &Ui) -> anyhow::Result<OAuthToken> {
        let mut config = OAuthConfig::new(path);
        if let Some(base) = &self.test_url {
            config.authorization_url = format!("{base}/authorize");
            config.token_url = format!("{base}/token");
        }
        let pending = SignIn::start(config).await?;
        let url = pending.authorization_url().to_owned();
        ui.line(format!("Sign in to Google: {url}"));
        let opened = tokio::task::spawn_blocking(move || webbrowser::open(url.as_str()))
            .await
            .context("opening the sign-in page")?;
        if let Err(error) = opened {
            tracing::warn!("could not open the browser: {error}; open the sign-in URL above");
        }
        Ok(pending.finish().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::decode_separator;

    #[test]
    fn separator_escapes_are_decoded_once_and_other_characters_are_preserved() {
        assert_eq!(decode_separator(r"\n\t\\"), "\n\t\\");
        assert_eq!(decode_separator(r"\\n\q\"), "\\n\\q\\");
        assert_eq!(decode_separator("فاصل\n"), "فاصل\n");
        assert_eq!(decode_separator(""), "");
    }
}
