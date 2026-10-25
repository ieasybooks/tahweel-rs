# User guide

See the [README](../README.md) for installation and basic usage, or the [CLI reference](cli.md) for every option. Every conversion requires `--recognizer drive-oauth` or `--recognizer drive-service-account`. Both use Google Drive OCR: tahweel uploads each page image as a Google Doc, exports its text, then deletes the temporary document.

## Google sign-in (`drive-oauth`)

On the first run, tahweel prints a Google sign-in URL and opens your browser. Choose an account and finish the consent screen; conversion starts when the browser returns to tahweel. Later runs reuse the saved sign-in and refresh it when needed.

```sh
tahweel book.pdf --recognizer drive-oauth
```

The default saved sign-in is `tahweel/google-token.json` inside your local data directory:

- Linux: `$XDG_DATA_HOME/tahweel/google-token.json`, or `~/.local/share/tahweel/google-token.json` when `XDG_DATA_HOME` is unset.
- macOS: `~/Library/Application Support/tahweel/google-token.json`.
- Windows: `%LOCALAPPDATA%\tahweel\google-token.json`.

Delete that file to sign out of tahweel. This removes the local sign-in; it does not sign your browser out of Google. To keep another account's sign-in separately, choose a different file with `--drive-token`. A missing file starts browser sign-in; an existing file selects the account saved there.

```sh
tahweel book.pdf --recognizer drive-oauth --drive-token accounts/research.json
```

If Google rejects the saved sign-in during setup, tahweel asks you to sign in again once. An unreadable or malformed token file is reported as an error; remove or replace it before retrying. If Google rejects the account during OCR, the run stops. Delete its token file and rerun to sign in again; completed pages can be resumed.

Sign-in needs a browser on the same machine as tahweel. The callback goes to a temporary port on `127.0.0.1`; opening the printed URL on a different computer does not reach tahweel. If the browser does not open automatically, paste the URL into a browser on the same machine. Sign-in waits up to five minutes. Service accounts are the practical choice for servers and machines without a browser.

## Service accounts (`drive-service-account`)

In the [Google Cloud console](https://console.cloud.google.com/), select or create a project. Under **APIs & Services → Library**, enable the **Google Drive API**. Under **IAM & Admin → Service Accounts**, create a service account, open its **Keys** tab, and choose **Add key → Create new key → JSON**. Save the downloaded JSON file and pass its path to tahweel.

```sh
tahweel book.pdf --recognizer drive-service-account --service-account key.json
tahweel books/ --recognizer drive-service-account --service-account key-a.json,key-b.json -r -o text
tahweel books/ --recognizer drive-service-account --service-account key-a.json --service-account key-b.json -r -o text
```

You can also use a comma-separated environment variable. Explicit `--service-account` options replace the environment list.

```sh
export TAHWEEL_SERVICE_ACCOUNTS=/path/to/key-a.json,/path/to/key-b.json
tahweel books/ --recognizer drive-service-account -r -o text
```

The default is eight page operations per account, including upload, export and deletion. `--drive-concurrency N` changes this limit for every supplied account. With keys for two accounts, eight per account permits up to sixteen page operations across the run. Work is shared across accounts page by page, including pages from a single PDF. Use keys for distinct accounts when adding capacity.

`--drive-concurrency` also applies to the single account selected by `drive-oauth`. Transient HTTP failures are retried with backoff; `--drive-attempts` defaults to five attempts per operation, including the first. Rate-limit responses have a separate bounded retry budget. Drive requests have a 180-second total deadline and a 20-second connection timeout. Lower concurrency when a slow connection cannot keep up with uploads.

## Inputs and outputs

Inputs can be PDF, JPG/JPEG and PNG files, or folders containing them. Each image is one document with one page. Every PDF page is rendered and recognized, including pages that already contain selectable text; tahweel does not extract the existing text layer. Arabic is the default OCR language (`-l ar`); use `-l en` or another language code to change the hint sent to Drive. Use `-l auto` to omit the hint and let Drive detect the language.

```sh
tahweel book.pdf cover.jpg page.png --recognizer drive-oauth
tahweel books/ --recognizer drive-oauth -r -o text -f txt,docx
tahweel book.pdf --recognizer drive-oauth -l en -f json
tahweel book.pdf --recognizer drive-oauth -l auto
```

Folders are scanned one level deep by default. `-r` / `--recursive` includes subfolders. Folder discovery recognizes extensions without regard to case, skips directory symlinks, and skips an output directory nested inside the input tree during recursive traversal. Multiple input files and folders can be supplied in one command.

### Output locations

Outputs use the input's name with its last extension replaced: `book.pdf` becomes `book.txt`, `book.docx` and `book.json`. All three formats are written by default. Select formats with `-f txt`, `-f txt,json`, or `-f all`.

| Mode | Destination | `-o` |
|---|---|---|
| `side-by-side` | Beside each input file | Rejected; this is the default without `-o` |
| `tree-to-tree` | A mirrored tree with all formats together | Required; this is the default with `-o` |
| `flat` | Every output directly in one folder | Required |

```sh
tahweel books/ --recognizer drive-oauth -r
tahweel books/ --recognizer drive-oauth -r -o out
tahweel books/ --recognizer drive-oauth -r --output-mode flat -o out
```

For `books/history/volume.pdf`, these commands write TXT to `books/history/volume.txt`, `out/history/volume.txt`, and `out/volume.txt`, respectively. DOCX and JSON use the same layout.

With several folder inputs, tree mode gives each root its own folder: `tahweel books/ papers/ --recognizer drive-oauth -r -o out` writes under `out/books/` and `out/papers/`. Root folders with the same name get numeric suffixes. Individually supplied files go directly in `out` in tree mode.

When inputs in the same run would share a destination, names gain `-2`, `-3`, and so on, ignoring case. For example, `book.pdf` and `book.png` can become `book.txt` and `book-2.txt`. Output directories take precedence over conflicting file names. Existing output files from earlier runs are overwritten, rather than causing a new suffix. Keep the same input set, order and output options when resuming, since changing them can change collision suffixes.

`--skip-existing` skips a document only when every requested output file exists. With `-f txt,json`, both files must exist; DOCX is irrelevant. If an output is missing, tahweel processes the document, reuses any matching checkpoint, and rewrites all requested formats. The skip check only checks filenames, not their contents or the language and recognizer that produced them. Use different output folders when keeping results for different settings.

### Output formats

TXT is UTF-8 with LF line endings. Pages are trimmed and joined with `\n\nPAGE_SEPARATOR\n\n` by default, without an extra separator after the final page. Set `--page-separator` to change it. The CLI decodes `\n` as a newline, `\t` as a tab, and `\\` as a literal backslash; other escapes stay literal. Quote the value so the shell passes the backslashes through.

```sh
tahweel book.pdf --recognizer drive-oauth -f txt --page-separator '\n\n---\n\n'
tahweel book.pdf --recognizer drive-oauth -f txt --page-separator ''
```

JSON is a pretty-printed UTF-8 array in page order, with page numbers starting at 1. Its shape matches tahweel.rb:

```json
[
  { "page": 1, "content": "First page\nSecond line" },
  { "page": 2, "content": "Next page" }
]
```

DOCX has one text paragraph per source page, with page breaks between source pages. Like tahweel.rb, it normalizes line endings, collapses repeated identical whitespace, and merges the shortest adjacent lines while the estimated page length exceeds 40 lines; a line longer than 80 characters counts as two. Remaining line breaks are preserved. This helps the text fit but does not guarantee the same pagination in every word processor.

Arabic-dominant paragraphs are right-aligned and marked right-to-left; other paragraphs are left-aligned and left-to-right. Digits keep the form returned by OCR. Text is 10 points, as in tahweel.rb. TXT and JSON retain the page text without DOCX line merging. None of the formats reconstructs the source's illustrations, columns or typography.

## Page images and PDFium

The defaults are 200 DPI for PDFs and lossless grayscale PNG for page images. Image inputs are oriented using their EXIF orientation and capped at a 2,339-pixel long edge, preserving aspect ratio and never enlarging smaller images. `--dpi` affects PDFs only; stored DPI metadata does not set the size of JPG or PNG inputs.

Each encoded image sent to Drive is capped at 2,000,000 bytes (2 MB). If PNG exceeds that cap, tahweel tries JPEG qualities 95, 85, 75 and 60 at the same dimensions. If it still does not fit, it shrinks the image proportionally and encodes at JPEG quality 75 until it fits. The cap applies to each prepared page image, not the size of the input PDF or image file.

Start with 200 DPI. Try a higher value for small print and compare a few representative pages before running a whole book. A lower value can reduce rendering and upload work. Higher DPI uses more memory and may trigger the image-size fallback; it did not improve aggregate accuracy in the [defaults evaluation](benchmarks/defaults-2026-10-25.md).

```sh
tahweel book.pdf --recognizer drive-oauth --dpi 250 -o trial
```

PDFs need the PDFium shared library at runtime. [Release archives](https://github.com/ieasybooks/tahweel-rs/releases/latest) include the matching library beside the executable and its license notices. Keep them together after extraction. Cargo installations and library applications obtain PDFium separately; builds do not download it. The pinned library is from [pdfium-binaries chromium/8111](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8111).

An explicit `--pdfium PATH` selects the library file; `TAHWEEL_PDFIUM` supplies a default for that option. Without either, tahweel looks beside its executable for `libpdfium.so` on Linux, `libpdfium.dylib` on macOS, or `pdfium.dll` on Windows, then tries the system library search path. An invalid explicit path is reported without falling back. A loading failure stops the run with the attempted locations and installation instructions. Image-only runs need no PDFium.

```sh
tahweel book.pdf --recognizer drive-oauth --pdfium /path/to/libpdfium.so
```

macOS downloads are unsigned and not notarized. Gatekeeper can block the executable or its adjacent library after a browser download. Follow **Privacy & Security → Open Anyway** for this download. If quarantine still blocks the library, and you have verified and chosen to use the release, remove the quarantine attribute from its extracted directory:

```sh
xattr -dr com.apple.quarantine "/path/to/tahweel-<tag>-<target>"
```

Replace the path with your extracted directory. The bundled PDFium needs macOS 13 or newer. See [Releasing](../.github/RELEASING.md#binary-archives) for platform requirements and archive contents.

## Resuming, cancelling and leftovers

The CLI saves each completed page in `tahweel/checkpoints` inside the user cache directory. This is `$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS, and `%LOCALAPPDATA%` on Windows. Checkpoints contain recognized text and are separate from final outputs. Rerun the same command after an interruption or failed page to reuse completed pages. A checkpoint is removed once its document's requested outputs have been written successfully.

Checkpoint matching uses the input's canonical path, file size and modification time, page preparation settings, recognizer name and language. Moving or modifying the input, changing `--dpi`, or choosing another recognizer or language starts a new checkpoint. A release that changes page preparation defaults also changes the match. Input contents are not hashed. Changing output formats, the TXT separator or output directory does not invalidate saved page text; neither does changing the Drive account or concurrency. Corrupt checkpoint records cause that checkpoint to be restarted. Delete the cache's `tahweel/checkpoints` directory while no run is active to discard saved page text.

Press Ctrl-C once to stop starting new pages and wait for running pages to finish, save their text and clean up their temporary Drive documents. The interrupted run exits with code 130. A second Ctrl-C exits immediately and can leave unfinished uploads or deletions. During initial setup, before pages have been sent, one Ctrl-C can stop immediately.

A page failure fails its document; failed OCR does not produce a partial final document with a missing or blank replacement page. Other documents continue unless the problem affects the whole run, such as an unusable recognizer. Successful pages remain available in the checkpoint. A failure does not remove outputs left by an earlier run, so omit `--skip-existing` when intentionally replacing those results.

Every uploaded Drive document is scheduled for deletion after export, including when export fails. Failed deletions are logged. A lost response, forced exit or network failure can leave a temporary document behind. At startup, tahweel attempts to delete files tagged by this app that are older than one hour in each selected account. It leaves newer and untagged files alone; cleanup failures are logged without preventing OCR.

## Speed and accuracy

The [defaults evaluation](benchmarks/defaults-2026-10-25.md) covered 177 pages from real scans and synthetic Arabic books, including mixed Arabic/English text. The chosen settings were 200 DPI, grayscale PNG, a 2 MB encoded-image cap and `ocrLanguage=ar`. Mean normalized character error rate (CER) over the full corpus was **11.390%**, and mean normalized word error rate (WER) was **22.518%**. These are measurements on that corpus, not expected error rates for every book.

Eight workers reached about **16 pages per minute per account** on the test connection. This measures the evaluation's Drive upload/export/delete workload, not a guarantee for a whole CLI run; local rendering, page size, network conditions and Drive latency affect throughput. Eight was the highest concurrency tested. The evaluation used a separate harness; its transport experiments are described in the linked report, while tahweel uses multipart uploads.

- Check a few representative pages before a large batch. Two-column pages and open-book spreads can interleave reading order; splitting columns or separating the two pages before conversion can help.
- Use clear, upright scans and keep the whole text area visible. Raising DPI cannot recover detail missing from the original scan.
- Lower `--drive-concurrency`, for example to `2` or `4`, when uploads stall on a slow or shared connection.
- Use `--skip-existing` to avoid reprocessing finished documents when restarting a batch. Matching checkpoints resume unfinished ones automatically.

## Help and shell completions

Conversion is the default command: `tahweel book.pdf --recognizer drive-oauth` and `tahweel convert book.pdf --recognizer drive-oauth` do the same thing. Use `-h` for brief help, `--help` for detailed help, or the [CLI reference](cli.md). Release archives include a [man page](tahweel.1), readable with `man ./docs/tahweel.1`. Help, completions and specification export work without credentials or PDFium.

```sh
tahweel completions zsh --install --dry-run
tahweel completions zsh --install
tahweel completions bash > tahweel.bash
```

Supported shells are `bash`, `elvish`, `fish`, `nu` (Nushell), `powershell` and `zsh`. `--dry-run` shows the destination and any setup instructions; after installation, follow any printed instructions for your shell. Installation preserves unrelated files and does not edit shell startup files. Completion scripts call tahweel directly, so no separate `usage` executable is needed.

`-q` / `--quiet` hides progress bars. `-v` adds debug logging and `-vv` adds trace logging; `RUST_LOG` overrides those logging filters. Exit codes are `0` for success, `1` for setup or other run errors, `2` for invalid arguments, `3` when some inputs or documents failed, and `130` for Ctrl-C.

## Reusable options and input lists

Use `@FILE` to read options and inputs from a UTF-8 file. For example, save this as `arabic.args`:

```text
--recognizer drive-oauth
--language ar
--format txt,docx
--output-dir text
--recursive
--skip-existing
```

```sh
tahweel @arabic.args books/
tahweel @arabic.args "book one.pdf" --output-dir another-folder --no-skip-existing
```

Argument files contain shell-style words: quotes preserve spaces and `#` starts a comment. They can include other argument files with `@relative/path.args`; those nested file paths resolve beside the containing file. Input, output and credential paths still resolve from the working directory. Cycles and nesting beyond 16 files are rejected. Use `@@` for a literal leading `@`, such as `@@book.pdf`. Shell commands and environment-variable substitutions are not executed in these files.

Repeated scalar options use the last value, so flags after `@FILE` override its settings. List options such as `--format` and `--service-account` accumulate. Boolean conversion options accept `--no-recursive`, `--no-quiet` and `--no-skip-existing` to override presets. Explicit `--service-account` is rejected with `drive-oauth`, and `--drive-token` is rejected with `drive-service-account`. The `TAHWEEL_SERVICE_ACCOUNTS` environment default is ignored by `drive-oauth`.

## CLI specification

`tahweel spec` prints the machine-readable [Usage](https://usage.jdx.dev/) KDL specification generated from the parser's declarations. The optional Usage CLI can turn it into Markdown or a man page:

```sh
tahweel spec > tahweel.usage.kdl
usage generate markdown -f tahweel.usage.kdl --html-encode --out-file reference.md
usage generate manpage -f tahweel.usage.kdl > tahweel.1
```

## Using it as a library

[`tahweel`](https://docs.rs/tahweel) supplies input discovery, page preparation, the pipeline and output writers. [`tahweel-drive`](https://docs.rs/tahweel-drive) supplies Google Drive OCR and authentication. The CLI package is `tahweel-cli`; its executable is `tahweel`.

Add the libraries and a Tokio runtime to your application:

```sh
cargo add tahweel tahweel-drive
cargo add tokio --features macros,rt-multi-thread,signal
cargo add tokio-util --features rt
```

This example reads a service-account key, converts one image, keeps the results in memory and reports progress through a closure. During conversion, Ctrl-C requests cancellation and then waits for the run to finish its active pages. It does not need PDFium because the input is an image.

```rust
use tahweel::{BoxError, DocumentStatus, Event, Input, OutputWriter, Tahweel};
use tahweel_drive::{DriveAuth, DriveConfig, DriveRecognizer, ServiceAccountKey};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let key = ServiceAccountKey::from_file("service-account.json")?;
    let recognizer =
        DriveRecognizer::new(DriveConfig::new(DriveAuth::ServiceAccounts(vec![key]))).await?;
    let cancellation = CancellationToken::new();
    let pipeline = Tahweel::builder(recognizer)
        .output(OutputWriter::new().formats([]))
        .cancellation_token(cancellation.clone())
        .build();
    let progress = |event: Event| eprintln!("{event:?}");
    let run = pipeline.run([Input::from("page.png")], &progress);
    tokio::pin!(run);
    let report = tokio::select! {
        result = &mut run => result?,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            cancellation.cancel();
            run.await?
        }
    };
    for document in &report.documents {
        if let DocumentStatus::Converted { pages, .. } = &document.status {
            for (index, text) in pages.iter().enumerate() {
                println!(
                    "{}: page {}\n{text}",
                    document.document.path.display(),
                    index + 1
                );
            }
        }
    }
    if report.cancelled || report.failures() > 0 {
        return Err(std::io::Error::other("conversion did not finish successfully").into());
    }
    Ok(())
}
```

`run` requires a Tokio runtime. Its report contains documents in input order, with each converted document's `pages` in page order. Ordinary input and document failures are in `report.failed_inputs` and `DocumentStatus::Failed`; check `report.failures()` as well as the returned `Result`. `report.cancelled` records graceful cancellation. Errors affecting the entire run are returned after active recognition calls have finished. Keep progress callbacks short because they run on the collecting task; use `NoProgress` when no callback is needed.

The library defaults to all three formats beside each input. An empty format list, as above, writes no final output files. Select formats with `OutputWriter::new().formats([tahweel::Format::Txt])`, and choose destinations with `OutputMode::TreeToTree(dir)` or `OutputMode::Flat(dir)`. Use `.recursive(true)` for subfolders. Library checkpoints are opt-in: set `.checkpoints(directory)` and a `.checkpoint_label("drive-service-account:ar")` identifying your recognition settings, and change the label when those settings change.

For PDF inputs, use `.pdfium_library(path)` or the executable-adjacent/system library search. The library itself does not read `TAHWEEL_PDFIUM` or credential environment variables. `DriveConfig` controls language and account concurrency: `language` defaults to `Some("ar".into())`; set it to `None` to let Drive detect the language, and use a distinct checkpoint label such as `"drive-service-account:auto"`. `PageOptions`, passed to `.page_options(options)`, controls rendering and encoding. For OAuth in an application, see the [Drive library documentation](https://docs.rs/tahweel-drive): the caller presents the sign-in URL, and the library handles the loopback callback and saved token refresh.

### Another OCR backend

Implement `tahweel::Recognizer` using `async-trait`. The trait requires `Send + Sync`; `recognize(&self, page: &PageImage)` returns `Result<Recognition, BoxError>`. `PageImage` contains encoded bytes, the format and pixel dimensions. Return text with LF line endings using `Recognition::new(text)`. Retries, rate limits and cleanup belong to the recognizer.

For example, this adapter targets an HTTP OCR service that accepts an image as its request body and returns UTF-8 plain text with LF line endings. Add `async-trait` and `reqwest` to the application. A 401 response means this example service has rejected the configured client's credentials, so the adapter stops the whole run.

```rust
use async_trait::async_trait;
use tahweel::{BoxError, PageImage, Recognition, Recognizer};

pub struct HttpRecognizer {
    pub client: reqwest::Client,
    pub url: String,
}

#[async_trait]
impl Recognizer for HttpRecognizer {
    async fn recognize(&self, page: &PageImage) -> Result<Recognition, BoxError> {
        let response = self
            .client
            .post(&self.url)
            .header("Content-Type", page.format.mime_type())
            .body(page.data.clone())
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(tahweel::Error::RecognizerUnavailable {
                source: std::io::Error::other("OCR credentials were rejected").into(),
            }
            .into());
        }
        let text = response.error_for_status()?.text().await?;
        Ok(Recognition::new(text))
    }

    fn max_image_bytes(&self) -> Option<usize> {
        Some(2_000_000)
    }

    fn max_in_flight(&self) -> usize {
        2
    }
}
```

Construct the adapter with your service URL and configured HTTP client, then pass it to `Tahweel::builder`. Set a request timeout on that client and adapt retry handling to your service. `max_image_bytes` tells the pipeline how small to make encoded images; it defaults to `None` for no byte limit. `max_in_flight` bounds simultaneous recognition calls across the whole run; it defaults to 1 and must be positive. The example's limits are illustrative, so use the limits of your own backend.

An ordinary recognition error fails its document. Return a boxed `tahweel::Error::RecognizerUnavailable { source }` for a problem that prevents the whole backend from continuing. The pipeline stops starting pages and waits for active calls before returning the error. For user cancellation, cancel the supplied `CancellationToken` and keep awaiting `run`; dropping the run future aborts recognition tasks instead of waiting for their cleanup.

## Differences from the Python and Ruby versions

- Choose `--recognizer` explicitly. Python used service accounts and Ruby used browser sign-in; this CLI supports both without switching between them automatically. Several positional inputs are accepted, and folder recursion requires `-r`.
- TXT uses tahweel.rb's `\n\nPAGE_SEPARATOR\n\n` default separator and JSON uses its `page`/`content` array. All three formats are selected by default. Python wrote TXT and DOCX, and its separator had one newline on each side. TXT and JSON now use LF line endings instead of retaining Drive's CRLF inside pages.
- Output modes follow tafrigh-rs, with all formats together. Python's separate ` - Tahweel TXT` and ` - Tahweel DOCX` folders are replaced by `side-by-side`, `tree-to-tree` and `flat`. Colliding names gain numeric suffixes. Skipping finished outputs is opt-in with `--skip-existing` and checks only the requested formats.
- DOCX uses Ruby's line merging and content-based alignment, with right-to-left marking added for Arabic. Python's Arabic-Indic digit conversion and `--docx-remove-newlines` option are not carried over. Text stays at Ruby's 10 points.
- A failed page fails its document and is reported; it is not silently omitted or replaced with blank text. Python already failed a document on an OCR error, but returned a successful process exit code. Ruby could omit failed page renders and silently swallow file failures. This CLI returns code 3 when some inputs or documents fail, and saves completed pages for resuming.
- PDFium replaces Poppler. The defaults are grayscale PNG at 200 DPI, compared with Python's color JPEG at 200 DPI and Ruby's PNG at 150 DPI. Standalone images are capped at a 2,339-pixel long edge, and prepared page images at 2 MB with a JPEG fallback.
- The OCR language defaults to an explicit hint (`ocrLanguage=ar`); use `-l auto` to let Drive detect it as both originals did. Page concurrency is bounded per account across the run. Temporary Drive documents are cleaned up after export failures too, with a startup pass for tagged leftovers older than an hour; the originals had no such startup cleanup.

## Development

The Rust toolchain is pinned in `rust-toolchain.toml`. Run `mise install` from the repository root to install the configured tools. The CLI is in `src/`; the core and Drive libraries are in `crates/tahweel` and `crates/tahweel-drive`. Follow [AGENTS.md](../AGENTS.md) for project conventions and the required formatting, lint, test, dependency and spelling checks.

```sh
cargo test --workspace --locked
cargo test --workspace --locked --doc
typos
```

Ordinary Drive tests use local mock servers and need no credentials. Tests requiring PDFium look for `TAHWEEL_TEST_PDFIUM` and skip with a message when it is unset. To include them, set it to your matching library file before running the workspace tests:

```sh
TAHWEEL_TEST_PDFIUM=/path/to/libpdfium.so cargo test --workspace --locked
```

Real Drive tests are ignored by default. Set `TAHWEEL_TEST_SERVICE_ACCOUNT` to a service-account JSON file and run them explicitly. They make real requests, delete the test uploads and also run startup cleanup of older tagged files in that account.

```sh
TAHWEEL_TEST_SERVICE_ACCOUNT=/path/to/key.json cargo test -p tahweel-drive --locked --test real_drive -- --ignored --nocapture --test-threads=1
```

After CLI changes, regenerate the reference and man page with `sh scripts/cli-docs.sh`; CI checks that they match the parser. See [Releasing](../.github/RELEASING.md) for Conventional Commits, versioning and release workflows.
