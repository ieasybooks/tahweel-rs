# Releasing

All three crates share one version and one `vX.Y.Z` tag. Use [Conventional Commits](https://github.com/googleapis/release-please): `fix:` bumps the patch version, `feat:` bumps the minor version, and `feat!:` or a `BREAKING CHANGE:` footer marks incompatible changes. Before 1.0, breaking changes also bump the minor version. Keep this convention in squash-merge titles and preserve breaking-change footers.

After changes reach `main`, release-please opens or updates one release PR. Its Rust strategy updates all three package versions, internal path-dependency requirements, `Cargo.lock`, and `CHANGELOG.md` together. `.release-please-manifest.json` records the release baseline. Review the version, lockfile, generated release notes, and checks before merging. Use each workspace package's actual name as its dependency key: release-please does not follow Cargo's `package` aliases. CI checks the dependency keys and matching versions.

Merging the release PR creates the GitHub release and dispatches `release.yml` and `publish.yml` on the new tag. Explicit dispatch is needed because releases created with `GITHUB_TOKEN` do not trigger release-event workflows. The repository's **Settings → Actions → General → Allow GitHub Actions to create and approve pull requests** setting must be enabled. No additional personal token is needed for release-please. If GitHub requests approval for checks on a bot-created release PR, approve those runs before merging.

## Checking and retrying workflows

Manual runs keep artifacts or check packages by default. They do not publish:

```sh
gh workflow run release.yml --ref main -f publish=false
gh workflow run publish.yml --ref main -f publish=false
```

The release workflow builds all five archives and smoke tests their extracted contents from a separate working directory. It runs `tahweel --version`, checks the adjacent PDFium library and notices, and runs `cargo test --release --locked -p tahweel` for the selected target with `TAHWEEL_TEST_PDFIUM` pointing at the extracted library. PDF tests may skip when the variable is absent during ordinary development; release runs supply it. The package workflow runs `cargo package --workspace --locked`, which verifies the contents of all three packages, including unpublished workspace dependencies.

To retry an existing release, select its `vX.Y.Z` tag and enable `publish` on the appropriate workflow. Both workflows require that the selected ref is a tag matching the CLI package version before publishing. Archives are attached with `--clobber`; crate versions already present on crates.io are skipped. Branch builds use `dev` in archive names, and tag builds use the selected tag even with publishing disabled.

## crates.io

Publish in dependency order: `tahweel`, then `tahweel-drive`, then `tahweel-cli`. `publish.yml` uses the GitHub `release` environment and crates.io trusted publishing with `id-token: write`.

The first publication of a new crate requires a crates.io API token in the repository or `release` environment secret `CARGO_REGISTRY_TOKEN`: trusted publishing can only be configured after the crate exists. Give that token permission to publish these three crates. After their first publication, configure each crate's trusted publisher for owner `ieasybooks`, repository `tahweel-rs`, workflow `publish.yml`, and environment `release`. Remove `CARGO_REGISTRY_TOKEN` once all three trusted publishers are configured; subsequent releases obtain a short-lived token through `rust-lang/crates-io-auth-action`.

## Binary archives

| Target | Runner | Archive |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | `.tar.gz` |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` | `.tar.gz` |
| `aarch64-apple-darwin` | `macos-15` | `.tar.gz` |
| `x86_64-apple-darwin` | `macos-15-intel` | `.tar.gz` |
| `x86_64-pc-windows-msvc` | `windows-2025` | `.zip` |

Each `tahweel-<tag>-<target>` archive contains a directory of the same name with:

- `tahweel` (Windows: `tahweel.exe`) beside `libpdfium.so`, `libpdfium.dylib`, or `pdfium.dll`.
- `README.md` and the project's `LICENSE`.
- `docs/cli.md` and `docs/tahweel.1` when present in the source tree.
- `PDFIUM-LICENSE` (the upstream package's `LICENSE`) and the complete upstream `licenses/` directory.
- `THIRD-PARTY-LICENSES`, generated from Rust dependencies with the pinned cargo-about version in `mise.toml`.

Each archive has a companion `.sha256` file containing its SHA-256 and filename. Keep the executable, PDFium library, and notices together when installing or redistributing it. Linux builds use Ubuntu 22.04 to keep the glibc requirement at 2.35 or older. The bundled PDFium libraries require macOS 13 or newer. Windows builds set `RUSTFLAGS=-C target-feature=+crt-static` and `AWS_LC_SYS_PREBUILT_NASM=1`, avoiding a separate MSVC runtime installation.

macOS downloads are unsigned and not notarized. Gatekeeper can block the executable or adjacent library after a browser download. Follow macOS's **Privacy & Security → Open Anyway** process for this download. If quarantine still blocks the library, and you have verified and chosen to use this release, remove the quarantine attribute from the extracted directory with `xattr -dr com.apple.quarantine /path/to/tahweel-<tag>-<target>`. CI tests loading the archive; it does not reproduce browser quarantine or prove notarization.

Cargo installations and library users obtain PDFium separately; Cargo builds never download it. Put the matching library beside the executable, pass `--pdfium PATH` (or `TAHWEEL_PDFIUM`), or install it on the system library search path. PDFium is needed only for PDF inputs.

## Updating PDFium

`.github/scripts/pdfium.py` pins [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8111) to `chromium/8111`, with the SHA-256 of each of the five assets. CI uses it for the PDF tests and `release.yml` uses it for the archives, so both always get the same verified library. The bundled PDFium code and third-party notices have their own licenses; the package script's MIT license does not replace them.

To update the pin, download the five new `pdfium-{linux-x64,linux-arm64,mac-arm64,mac-x64,win-x64}.tgz` assets, calculate each SHA-256, inspect their library paths and full notices directory, then update the version and all five hashes in the script together. Check compatibility with the core crate's `pdfium-render` API feature. Run the release workflow with publishing disabled, inspect all five archives and dependency/load-command requirements, and verify the core PDF tests ran against the extracted libraries. Recheck macOS and glibc minimums before changing the documented platform requirements.

## Rust dependency notices

`about.toml` includes dependencies for every target and excludes development-only dependencies. Its `accepted` list must match `deny.toml`'s `licenses.allow`. When dependencies or allowed licenses change, update both configurations and regenerate notices locally:

```sh
mise install
mkdir -p target
cargo about generate --locked --all-features --workspace --fail about.hbs --output-file target/THIRD-PARTY-LICENSES
```

Mise uses the pinned cargo-binstall to install cargo-about from an upstream binary when available, and builds it from source otherwise (including Intel macOS, for which cargo-about 0.9.2 has no upstream binary).

The template includes each crate's name, version, source download URL, and full license texts, preserving distinct copyright notices. CI checks the license lists and generates the file on every PR; the release workflow generates one file covering all target dependency graphs and copies it into every archive. Do not commit generated output. These archive notices do not automatically add a notice file to crates.io packages; keep any package-specific notices required for source distribution.
