"""Install the pinned PDFium test runtime; builds and published crates never download it."""

import hashlib
import os
from pathlib import Path
import platform
import shutil
import sys
import tarfile
import tempfile
import urllib.request


ASSETS = {
    "linux-x64": (
        "bdc60126399f39fe768566a18d4be055a7b3681bc9e248e8a6656b4f6cae0aa2",
        "lib/libpdfium.so",
    ),
    "linux-arm64": (
        "042de247138f96341534cccf24ed21c2550a7245b936bb4a60d2340120000f05",
        "lib/libpdfium.so",
    ),
    "mac-arm64": (
        "46c674298c60123f1195c2a03c65ee08c4783c84874ce4f5b3a054abb3ecaf30",
        "lib/libpdfium.dylib",
    ),
    "mac-x64": (
        "e7fece4693f65a7e47f84fa38099068d573d4dcbd46b1a3c8d2c26581c308466",
        "lib/libpdfium.dylib",
    ),
    "win-x64": (
        "3452d11031050ec65e57f180215b57b9e6d465e3fbedafa8a77ad90885c7d23e",
        "bin/pdfium.dll",
    ),
}


def main():
    system = {"linux": "linux", "darwin": "mac", "win32": "win"}[sys.platform]
    arch = {"x86_64": "x64", "amd64": "x64", "arm64": "arm64", "aarch64": "arm64"}[
        platform.machine().lower()
    ]
    target = f"{system}-{arch}"
    digest, relative_library = ASSETS[target]
    directory = Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir())) / "tahweel-pdfium-8111"
    directory.mkdir(parents=True, exist_ok=True)
    print(f"Installing PDFium chromium/8111 for {target}; disk: {shutil.disk_usage(directory)}")
    url = f"https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8111/pdfium-{target}.tgz"
    archive = directory / "pdfium.tgz"
    with urllib.request.urlopen(url, timeout=120) as response, archive.open("wb") as output:
        shutil.copyfileobj(response, output)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != digest:
        raise ValueError("PDFium archive checksum does not match the pinned release")
    with tarfile.open(archive) as package:
        package.extractall(directory, filter="data")
    archive.unlink()
    library = directory / relative_library
    if not library.is_file():
        raise FileNotFoundError(library)
    with Path(os.environ["GITHUB_ENV"]).open("a", encoding="utf-8") as environment:
        environment.write(f"TAHWEEL_TEST_PDFIUM={library}\n")
    print(f"PDF tests will use {library}")


if __name__ == "__main__":
    main()
