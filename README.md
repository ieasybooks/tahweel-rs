# tahweel

Convert PDF, JPEG and PNG books into TXT, DOCX and JSON with Google Drive OCR. A Rust rewrite of [tahweel](https://github.com/ieasybooks/tahweel) and [tahweel.rb](https://github.com/ieasybooks/tahweel.rb), with Arabic as the default language. Every page is recognized, even when a PDF already has a text layer.

## Install

Download the archive for your platform from the [latest release](https://github.com/ieasybooks/tahweel-rs/releases/latest), extract it, and put `tahweel` on your `PATH`. Keep the included PDFium library beside the executable. Archives support Linux x86_64/arm64, macOS arm64/x86_64 and Windows x86_64.

Or install with Cargo (Rust 1.99 or newer):

```sh
cargo install tahweel-cli --locked
```

Cargo installations need a [PDFium shared library](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8111) for PDF inputs: put `libpdfium.so`, `libpdfium.dylib` or `pdfium.dll` beside `tahweel`, or pass `--pdfium PATH` (also `TAHWEEL_PDFIUM`). System library paths are also searched. Image inputs need no PDFium.

## Usage

Choose `--recognizer drive-oauth` or `drive-service-account` for each run.

Google sign-in opens your browser on the first run and saves the sign-in for later:

```sh
tahweel book.pdf --recognizer drive-oauth
tahweel books/ --recognizer drive-oauth -r -o text -f txt,docx
```

Use `--drive-token FILE` to choose the saved sign-in file. The default is `tahweel/google-token.json` in your local data directory, such as `~/.local/share/tahweel/google-token.json` on Linux. Delete that file to sign out.

For service accounts, enable the Google Drive API in your Google Cloud project and use its service-account JSON keys:

```sh
export TAHWEEL_SERVICE_ACCOUNTS=/path/to/key.json

tahweel book.pdf --recognizer drive-service-account
tahweel books/ --recognizer drive-service-account --service-account key-a.json,key-b.json -r -o text
```

You can repeat `--service-account` or comma-separate paths. Explicit keys override the environment list. Use `-l en` for English or another OCR language code.

Outputs go beside the input by default. Use `-o` to mirror a folder, `--output-mode flat -o DIR` for one output folder, and `-r` to include subfolders. All three formats are written by default. `--skip-existing` skips documents only when every requested output exists; otherwise outputs are overwritten.

Completed pages are saved in `tahweel/checkpoints` under your user cache directory and resumed after an interrupted or failed run. Ctrl-C stops new pages and waits for running pages to finish deleting their temporary Drive files; a second Ctrl-C exits immediately.

## Library

Add the libraries and a Tokio runtime to an application:

```sh
cargo add tahweel tahweel-drive
cargo add tokio --features macros,rt-multi-thread
```

```rust
use tahweel::{DocumentStatus, Input, NoProgress, OutputWriter, Tahweel};
use tahweel_drive::{DriveAuth, DriveConfig, DriveRecognizer, ServiceAccountKey};

let key = ServiceAccountKey::from_file("service-account.json")?;
let recognizer = DriveRecognizer::new(DriveConfig::new(DriveAuth::ServiceAccounts(vec![key]))).await?;
let pipeline = Tahweel::builder(recognizer).output(OutputWriter::new().formats([])).build();
let report = pipeline.run([Input::from("page.png")], &NoProgress).await?;
for document in &report.documents {
    if let DocumentStatus::Converted { pages, .. } = &document.status {
        println!("{}", pages.join("\n"));
    }
}
```

An empty format list keeps the recognized pages in memory instead of writing files. See the [library guide](https://github.com/ieasybooks/tahweel-rs/blob/main/docs/guide.md#using-it-as-a-library) for progress, cancellation, OAuth and custom OCR backends.

## Documentation

- [User guide](https://github.com/ieasybooks/tahweel-rs/blob/main/docs/guide.md): sign-in, output layouts, resuming, defaults and library examples.
- [Defaults evaluation](docs/benchmarks/defaults-2026-10-25.md): Arabic page accuracy, image sizing and Drive request measurements.
- [CLI reference](docs/cli.md): every option, example and exit code. Also available through `tahweel --help`.
- [Man page](docs/tahweel.1): included in release archives.
- API documentation: [`tahweel`](https://docs.rs/tahweel) and [`tahweel-drive`](https://docs.rs/tahweel-drive).

Install shell completions, then follow any printed setup instructions (replace `zsh` with your shell):

```sh
tahweel completions zsh --install
```

Use `@FILE` to load reusable options and input lists from shell-style words. Boolean conversion options accept a `--no-` prefix to override presets.

## Development

Install the configured tools with mise, then run the checks from the repository root:

```sh
mise install
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny --locked check
```

Tests use local Drive mocks; PDF tests use `TAHWEEL_TEST_PDFIUM` and skip when it is unset. After changing CLI declarations, regenerate the CLI reference with `sh scripts/cli-docs.sh`; CI checks that it is current. See [AGENTS.md](AGENTS.md) for project conventions.

## License

MIT. Release archives also include PDFium's license notices.
