# HayakuReader

Windows 本地阅读器，使用 Rust 构建。作者为 **drliuhuan**。

- [项目介绍](docs/intro.zh.md)
- [Project introduction](docs/intro.en.md)
- [下载 Windows x64 版](https://github.com/drliuhuan/HayakuReader/releases/latest)
- [需求文档](HayakuReader-需求文档.md)

## 安装和使用

下载 `HayakuReader.exe` 后双击启动。首次运行可安装到 `%LOCALAPPDATA%\Programs\HayakuReader`。确认后程序会设置所选关联并启动已安装副本。已安装用户可以在阅读设置中调整文件关联和自动更新。Windows 已为某个格式保存默认应用选择时，可在系统的「默认应用」中改选 HayakuReader。

标签页和阅读进度默认在下次启动时恢复，可以在设置中关闭此功能。后台标签页闲置五分钟，或移出最近五个标签页后会休眠。阅读进度、批注和设置保存在 `%LOCALAPPDATA%\HayakuReader`。

发布包只包含一个 EXE。运行需要 Windows 的 WebView2 Runtime。

## 构建

使用 Windows x64 的 Rust MSVC 目标、Microsoft C++ 链接工具和 Windows SDK，以及 Node.js。运行以下命令生成 `dist\HayakuReader.exe`。

```powershell
rustup target add x86_64-pc-windows-msvc
./scripts/package-windows.ps1
```

PDF 使用随应用编入的 PDF.js。Word 使用 Rust 的 docboss 分页和渲染，Kindle 由 ebook-rs 解析，Markdown 由 pulldown-cmark 渲染。LibreOffice、calibre 和 Calibri 字体均无需安装。

## 发布

1. 先更新 `docs/intro.zh.md` 和 `docs/intro.en.md`，它们同时是 GitHub 介绍和应用首页的离线副本。
2. 更新 Cargo 版本，构建并验证单文件 EXE。
3. 将源码和介绍推送到 GitHub，然后创建同版本的 Release。唯一的下载资产命名为 `HayakuReader.exe`。
