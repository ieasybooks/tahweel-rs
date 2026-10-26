//! Compare raw Google Drive exports of one uploaded page image.

use std::{
    env,
    ffi::OsString,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tahweel::{ImageFormat, PageImage};
use tahweel_drive::{DriveAuth, DriveConfig, DriveRecognizer, OAuthToken, ServiceAccountKey};

const USAGE: &str = "Usage: cargo run --example compare_exports -- IMAGE NEW_OUTPUT_DIR \\
  (--service-account KEY.json | --drive-token TOKEN.json) [--language ar]

IMAGE must be a JPEG or PNG no larger than 2,000,000 bytes. Its bytes are uploaded unchanged.
The output directory must not exist. --drive-token loads an existing saved Google sign-in.
Use --language auto to omit the OCR language hint.";

const FORMATS: [(&str, &str); 5] = [
    ("text/plain", "drive.txt"),
    ("text/html", "drive.html"),
    ("application/zip", "drive.html.zip"),
    ("text/markdown", "drive.md"),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "drive.docx",
    ),
];

struct Args {
    input: PathBuf,
    output: PathBuf,
    credentials: Credentials,
    language: String,
}

enum Credentials {
    ServiceAccount(PathBuf),
    OAuth(PathBuf),
}

fn parse_args(words: impl IntoIterator<Item = OsString>) -> Result<Args> {
    let mut words = words.into_iter();
    let mut paths = Vec::new();
    let mut credentials = None;
    let mut language = String::from("ar");
    let mut positional = false;
    while let Some(word) = words.next() {
        if positional {
            paths.push(PathBuf::from(word));
            continue;
        }
        match word.to_str() {
            Some("--") => positional = true,
            Some(flag @ ("--service-account" | "--drive-token")) => {
                let path = PathBuf::from(words.next().context("credential option needs a file")?);
                if credentials.is_some() || path.as_os_str().is_empty() {
                    bail!("provide exactly one nonempty --service-account or --drive-token file");
                }
                credentials = Some(if flag == "--service-account" {
                    Credentials::ServiceAccount(path)
                } else {
                    Credentials::OAuth(path)
                });
            }
            Some("-l" | "--language") => {
                language = words
                    .next()
                    .context("--language needs a value")?
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("language must be UTF-8"))?;
                if language.trim().is_empty() {
                    bail!("language must not be empty");
                }
            }
            Some(flag) if flag.starts_with('-') => bail!("unknown option: {flag}"),
            _ => paths.push(PathBuf::from(word)),
        }
    }
    if paths.len() != 2 || paths.iter().any(|path| path.as_os_str().is_empty()) {
        bail!("provide an image path and a new output directory\n{USAGE}");
    }
    Ok(Args {
        input: paths.remove(0),
        output: paths.remove(0),
        credentials: credentials.context("choose --service-account or --drive-token")?,
        language,
    })
}

fn write_manifest(directory: &Path, manifest: &Value) -> Result<()> {
    fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(manifest)?,
    )
    .context("writing manifest.json")
}

fn read_page(path: &Path) -> Result<PageImage> {
    let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if data.len() > 2_000_000 {
        bail!("image exceeds 2,000,000 bytes; resize or re-encode it before this experiment");
    }
    let image_format = image::guess_format(&data).context("detecting the image format")?;
    let format = match image_format {
        image::ImageFormat::Jpeg => ImageFormat::Jpeg,
        image::ImageFormat::Png => ImageFormat::Png,
        _ => bail!("the experiment accepts one JPEG or PNG image"),
    };
    let (width, height) = image::ImageReader::with_format(Cursor::new(&data), image_format)
        .into_dimensions()
        .context("reading image dimensions")?;
    Ok(PageImage::new(data, format, width, height))
}

async fn compare(args: Args) -> Result<()> {
    let page = read_page(&args.input)?;
    let auth = match args.credentials {
        Credentials::ServiceAccount(path) => {
            DriveAuth::ServiceAccounts(vec![ServiceAccountKey::from_file(path)?])
        }
        Credentials::OAuth(path) => DriveAuth::OAuth(OAuthToken::load(path)?),
    };
    let mut config = DriveConfig::new(auth);
    config.language = (args.language != "auto").then_some(args.language);
    config.concurrency_per_account = 1;

    fs::create_dir(&args.output).with_context(|| {
        format!(
            "creating {}; choose a directory that does not exist",
            args.output.display()
        )
    })?;
    let input_file = format!("input.{}", page.format.extension());
    fs::write(args.output.join(&input_file), &page.data)
        .context("saving the exact uploaded image")?;
    let mut manifest = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "status": "pending",
        "source": args.input,
        "uploaded_image": {
            "file": input_file, "mime_type": page.format.mime_type(),
            "width": page.width, "height": page.height, "bytes": page.data.len(),
            "preprocessing": "none; original bytes uploaded unchanged"
        },
        "ocr_language": config.language,
        "same_google_document": true,
        "exports": FORMATS.map(|(mime, file)| json!({
            "mime_type": mime, "file": file, "status": "not_attempted"
        }))
    });
    write_manifest(&args.output, &manifest)?;
    let mime_types = FORMATS.map(|(mime, _)| mime);
    let result = async {
        let recognizer = DriveRecognizer::new(config).await?;
        recognizer.export_page_formats(&page, &mime_types).await
    }
    .await;
    let results = match result {
        Ok(results) => results,
        Err(error) => {
            manifest["status"] = json!("failed");
            manifest["error"] = json!(error.to_string());
            write_manifest(&args.output, &manifest)?;
            return Err(error.into());
        }
    };

    let mut failures = 0;
    for (index, ((_, file), result)) in FORMATS.iter().zip(results).enumerate() {
        let entry = &mut manifest["exports"][index];
        let error = match result {
            Ok(bytes) => {
                entry["bytes"] = json!(bytes.len());
                fs::write(args.output.join(file), &bytes)
                    .err()
                    .map(|error| format!("could not save export: {error}"))
            }
            Err(error) => Some(error.to_string()),
        };
        if let Some(error) = error {
            failures += 1;
            entry["status"] = json!("failed");
            entry["error"] = json!(error);
            eprintln!("{file}: {error}");
        } else {
            entry["status"] = json!("saved");
            println!("{file}: {} bytes", entry["bytes"]);
        }
    }
    manifest["status"] = json!(if failures == 0 {
        "complete"
    } else {
        "partial_failure"
    });
    write_manifest(&args.output, &manifest)?;
    if failures > 0 {
        bail!(
            "{failures} export(s) failed; see {}/manifest.json",
            args.output.display()
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let words: Vec<_> = env::args_os().skip(1).collect();
    if words.iter().any(|word| word == "--help" || word == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let args = match parse_args(words) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    match compare(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
