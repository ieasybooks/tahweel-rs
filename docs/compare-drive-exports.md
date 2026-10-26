# Compare Google Drive export formats

This opt-in experiment checks whether Google preserves useful paragraph, list or style information when OCR text is exported in a richer format. It uploads one page image as one Google Doc, requests five exports from that same document, and attempts to delete the temporary document after all exports. Each format is attempted independently, so an unsupported export remains visible as a failure while the other results are saved. Normal `tahweel` conversion and its output formats are unchanged.

Run the example from a checkout with the pinned Rust toolchain. Supply one JPEG or PNG, a new output directory, and either a service-account key or an existing saved Google sign-in:

```sh
cargo run --locked --example compare_exports -- page.jpg export-comparison \
  --service-account /path/to/key.json

cargo run --locked --example compare_exports -- page.jpg another-comparison \
  --drive-token /path/to/google-token.json
```

The output directory must not already exist, and its parent directory must exist. The example loads the selected credentials explicitly; it does not start browser sign-in or look for credentials in environment variables. To create a saved sign-in first, use the normal CLI with `--recognizer drive-oauth --drive-token FILE`. The usual Drive startup cleanup of this app's tagged files older than one hour also runs.

Arabic is the default OCR language hint. Pass `-l en` or `--language en` for another language, or `--language auto` to omit the hint.

## Input and output

The original image bytes are uploaded unchanged. Unlike the normal conversion pipeline, this example does not apply EXIF rotation, resize, convert to grayscale or re-encode the image. Use an upright JPEG or PNG no larger than 2,000,000 bytes; prepare a smaller image beforehand if needed. This keeps the input identical across the five exports. PDF input is outside this experiment; export a representative page image first.

| File | Contents |
| --- | --- |
| `input.jpg` or `input.png` | Exact image bytes uploaded to Drive. |
| `drive.txt` | Raw `text/plain` export, without tahweel's text cleanup. |
| `drive.html` | Raw `text/html` export. |
| `drive.html.zip` | Raw `application/zip` export, including the HTML archive's assets. |
| `drive.md` | Raw `text/markdown` export. |
| `drive.docx` | Raw `application/vnd.openxmlformats-officedocument.wordprocessingml.document` export. |
| `manifest.json` | Input provenance, language hint, requested MIME types, per-format results and byte counts or errors. |

These MIME types are listed in Google's [export-format reference](https://developers.google.com/workspace/drive/api/guides/ref-export-formats). The example does not substitute one format for another. A failed format has a manifest entry without a successfully saved file, and the process exits unsuccessfully after preserving the other exports. Setup or upload failures are also recorded in the manifest once the output directory has been created. Request errors omit Google's response bodies and credential contents.

## What to compare

Open the HTML and DOCX and inspect the editable OCR text, since richer exports can also contain the original page image. Hide or ignore that embedded scan before judging the result. Compare paragraph boundaries, numbered lists, headings and wrapped lines against the scan, including the HTML's paragraph, line-break and list elements. Check whether a new paragraph corresponds to a real paragraph in the source or just a printed line; a richly styled document does not by itself establish correct layout.

For structural inspection, list the HTML archive or inspect the DOCX XML:

```sh
unzip -l export-comparison/drive.html.zip
unzip -p export-comparison/drive.docx word/document.xml
```

The experiment's `drive.docx` is Google's export. The normal CLI's DOCX is generated locally from plain OCR text, so it is not equivalent. Likewise, normal text recognition removes a leading BOM and underscore separator, normalizes line endings and trims the result; `drive.txt` preserves the original export bytes. Use several pages with different layouts before deciding whether the richer export improves formatting enough for the production pipeline.
