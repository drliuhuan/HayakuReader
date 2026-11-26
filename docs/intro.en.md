# HayakuReader

HayakuReader is a local Windows reader built in Rust by **drliuhuan**. Open PDF, EPUB, TXT, AZW3, AZW, MOBI, DOC, DOCX, HTML, and Markdown. Copy selected text and keep highlights, notes, and bookmarks.

- Switch between documents in tabs. Idle and less recently used tabs sleep and resume when selected.
- Reading positions and annotations stay on your computer. You can restore your tabs on startup.
- The single EXE supports per-user installation, file associations, and updates from GitHub.

Starting with v0.2.1, HTM and HTML are unchecked by default. You can still select both formats during installation or in settings after installing.

v0.2.2 shows update download progress and speed, offers a manual GitHub download after a failure, and asks you to close the previous process if it blocks an update.

v0.2.4 creates Start Menu and Desktop shortcuts on installation and dismisses the splash screen immediately once an associated file finishes loading. It adds global shortcuts (`Ctrl+O/F/B/W/Tab`, `Ctrl+wheel` zoom, `F11` focus mode), floating settings and search panels with match navigation, 4-color quick highlights and in-page annotation editing, perceptual dark-mode smart page inversion (preserving vivid colors and inserted images, lifting dark colors, and inverting full-page scanned PDFs), Markdown/HTML Rendered and Code view switching (with line numbers and syntax highlighting), Markdown annotation export, start-page recent history with delete/clear, HTTP Range loading for large PDFs, and nested EPUB/Kindle outlines with fragment anchors.

v0.2.5 further improves speed and memory efficiency with LRU page canvas/bitmap eviction for PDF and Word, scroll and highlight hot-path optimizations, a zero-allocation smart dark-mode LUT, zero-copy static asset serving, and Thin LTO release builds.

Complex Word layouts may differ from the originals. Kindle files protected by DRM or compressed with HUFF/CDIC are currently unsupported. OCR and Markdown editing are planned for later releases.

GitHub repository [github.com/drliuhuan/HayakuReader](https://github.com/drliuhuan/HayakuReader)

© 2026 drliuhuan · Apache-2.0
