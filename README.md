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

## Documentation

- [User guide](https://github.com/ieasybooks/tahweel-rs/blob/main/docs/guide.md): sign-in, output layouts, resuming, defaults and library examples.
- [CLI reference](docs/cli.md): all options, examples and exit codes; also available with `tahweel --help`.
- [Man page](docs/tahweel.1).
- [Defaults evaluation](docs/benchmarks/defaults-2026-10-25.md): Arabic page accuracy, image sizing and Drive request measurements.
- [Compare Drive exports](docs/compare-drive-exports.md): an opt-in experiment with raw TXT, HTML, Markdown and DOCX from the same OCR document.
- [Core library](https://docs.rs/tahweel) and [Drive recognizer](https://docs.rs/tahweel-drive).

Install shell completions, then follow any printed setup instructions (replace `zsh` with your shell):

```sh
tahweel completions zsh --install
```

Use `@FILE` to load reusable options and input lists from shell-style words. Boolean conversion options accept a `--no-` prefix to override presets.

## Development

Install the configured tools with `mise install`, then follow [AGENTS.md](AGENTS.md) for checks. Generate the CLI reference with `sh scripts/cli-docs.sh`; CI checks that it is current. Tests use local Drive mocks; PDF tests use `TAHWEEL_TEST_PDFIUM` and skip when it is unset.

## License

MIT. Release archives also include PDFium's license notices.
