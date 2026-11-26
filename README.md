# HayakuReader

HayakuReader 是用 Rust 构建的 Windows 本地阅读器。支持 PDF、EPUB、TXT、AZW3、AZW、MOBI、DOC、DOCX、HTML 和 Markdown。

## 构建与启动

安装 Rust 工具链、目标平台的链接器及 Node.js。在项目目录运行：

```powershell
npm.cmd ci
cargo run --release --locked
```

运行 `./scripts/package-windows.ps1` 会在 `dist` 下生成可运行的 Windows 目录。双击目录中的 `HayakuReader.exe` 即可启动，保留目录内的配套文件。

也可以把本地文件路径作为第一个参数。前端和 PDF 渲染资源在编译时打包进 EXE，运行时需要 Windows 的 WebView2 Runtime。Node.js 仅用于准备构建资源。

## 使用

- 通过「打开文件」或拖拽开始阅读。「最近阅读」可恢复阅读位置，也可重新关联已移动的文件。
- 选中文字后可复制、高亮或创建笔记。「批注」侧栏可修改、删除或跳转到对应内容。
- 阅读进度、批注和设置保存在 `%LOCALAPPDATA%\HayakuReader`。
- Word 文件在项目内使用 Rust 解码和分页，按本机字体情况替换缺失字体。复杂版式可能和 Microsoft Word 不同。
- Kindle 文件在项目内解码。目前支持未加密的常见 MOBI 和 KF8 文件。受 DRM 保护或采用 HUFF/CDIC 压缩的文件会显示相应提示。
- 支持 Markdown 标题目录、表格、代码块和本地图片。Markdown 编辑、扫描件 OCR 和批注导出列入后续版本。

各格式目标见 [需求文档](HayakuReader-需求文档.md)。

## 文档引擎

PDF 使用随应用打包的 PDF.js。DOC 和 DOCX 使用 Rust 的 docboss 解析并渲染，保留可选择的文本层。Kindle 格式由 Rust 的 ebook-rs 解析，Markdown 由 pulldown-cmark 渲染。EPUB、TXT 和 HTML 由项目内的阅读管线处理。格式处理全程在本地完成。
