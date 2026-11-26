# HayakuReader

HayakuReader is a local Windows reader built in Rust by **drliuhuan**. Open PDF, EPUB, TXT, AZW3, AZW, MOBI, DOC, DOCX, HTML, and Markdown. Copy selected text and keep highlights, notes, and bookmarks.

- Switch between documents in tabs. Idle and less recently used tabs sleep and resume when selected.
- Reading positions and annotations stay on your computer. You can restore your tabs on startup.
- The single EXE supports per-user installation, file associations, and updates from GitHub.

Starting with v0.2.1, HTM and HTML are unchecked by default. You can still select both formats during installation or in settings after installing.

v0.2.2 shows update download progress and speed, offers a manual GitHub download after a failure, and asks you to close the previous process if it blocks an update.

Complex Word layouts may differ from the originals. Kindle files protected by DRM or compressed with HUFF/CDIC are currently unsupported. OCR, Markdown editing, and annotation export are planned for later releases.
