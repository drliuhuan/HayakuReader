# HayakuReader

HayakuReader is a local Windows document reader built in pure Rust by **drliuhuan**. Powered by a native GPU rendering engine with no WebView2 dependency, it opens PDF, EPUB, TXT, AZW3, AZW, MOBI, DOC, DOCX, HTML, XML, and Markdown with text selection, bookmarks, full-text search, and comfortable dark-mode reading.

- **Native multi-format rendering**: Embedded PDFium for crisp PDF rendering; native Word pagination with dynamic CJK font matching; native reflowable layout supporting Ruby annotations, inline/display LaTeX math, and syntax-highlighted code mode with line numbers.
- **Tabs & power-saving sleep**: Switch between open documents with arrow-navigated tabs that automatically scroll to the active tab. Background tabs idle for 5 minutes or outside the 5 most recently used automatically sleep to free memory; on restart, only the active tab loads immediately while others resume on demand.
- **Flexible reading controls**: Light and dark themes, smart dark-mode inversion for PDF/Word (preserving vivid colors and illustrations while inverting scanned pages), draggable reading-width margins for reflowable documents, auto-hiding sidebar, and `F11` focus mode.
- **Offline & portable**: Reading progress, bookmarks, and preferences stay local. The single EXE supports per-user installation, file associations, Start Menu / Desktop shortcuts, and GitHub update checks.

- **v0.3.1 reading fixes**: Page through documents by clicking the scrollbar track, restore reading positions when switching tabs, display EPUB SVG illustrations and metadata covers, and keep the correct contents entry highlighted after navigation.

### Shortcuts

- `Ctrl+O` Open · `Ctrl+F` Find · `Ctrl+B` Pin / auto sidebar
- `Ctrl+W` Close tab · `Ctrl+Tab` Next tab · `F11` Focus mode
- `Ctrl+wheel` / `Ctrl+±` / `Ctrl+0` Zoom & reset · `Ctrl+A` Select document · `Ctrl+C` Copy

### Notes

Complex Word layouts may differ from the originals. Kindle files protected by DRM or compressed with HUFF/CDIC are currently unsupported. OCR and Markdown editing are planned for later releases.

GitHub repository [github.com/drliuhuan/HayakuReader](https://github.com/drliuhuan/HayakuReader)

© 2026 drliuhuan · Apache-2.0
