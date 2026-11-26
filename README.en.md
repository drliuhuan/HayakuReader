# HayakuReader

[简体中文](README.md) | **English**

A local Windows document reader built in Rust by **drliuhuan**.

- [Project introduction](docs/intro.en.md)
- [中文项目介绍](docs/intro.zh.md)
- [Download Windows x64](https://github.com/drliuhuan/HayakuReader/releases/latest)

## Installation and Usage

Download `HayakuReader.exe` and double-click to launch. On first run, you can install it to `%LOCALAPPDATA%\Programs\HayakuReader`. Confirming sets the selected file associations, creates Start Menu and Desktop shortcuts, and launches the installed copy. Installed users can adjust file associations and automatic updates in Reading Settings. If Windows has already saved a default app for a file type, switch it to HayakuReader in Windows Default apps.

Open tabs and reading positions are restored on the next launch by default; this can be turned off in Settings. Background tabs hibernate after five minutes of inactivity or when moved beyond the five most recent tabs. Reading positions, annotations, and settings are stored in `%LOCALAPPDATA%\HayakuReader`.

The release is a single standalone EXE and requires the Windows WebView2 Runtime.

## Building

Requires the Windows x64 Rust MSVC target, Microsoft C++ build tools, the Windows SDK, and Node.js. Run the following commands to produce `dist\HayakuReader.exe`:

```powershell
rustup target add x86_64-pc-windows-msvc
./scripts/package-windows.ps1
```

PDF rendering uses the bundled PDF.js. Word documents are laid out and rendered in Rust via `docboss`, Kindle formats are parsed with `ebook-rs`, and Markdown is rendered with `pulldown-cmark`. No LibreOffice, calibre, or Calibri installation is required.

## Releasing

1. Update `docs/intro.zh.md` and `docs/intro.en.md`, which serve as both the GitHub introductions and the offline copies embedded into the app's start page.
2. Bump the version in `Cargo.toml`, build, and verify the single-file EXE.
3. Push the source and introduction docs to GitHub, then create a matching Release with the single asset named `HayakuReader.exe`.
