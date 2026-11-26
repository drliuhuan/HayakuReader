# HayakuReader

HayakuReader 是 **drliuhuan** 用 Rust 构建的 Windows 本地阅读器。基于纯 Rust 原生 GPU 渲染架构，无需依赖 WebView2，开箱即用。支持打开 PDF、EPUB、TXT、AZW3、AZW、MOBI、DOC、DOCX、HTML、XML 和 Markdown，支持复制文字、书签管理、正文搜索与深色护眼阅读。

- **多格式原生渲染**：内嵌 PDFium 高清渲染 PDF；Word 原生分页并动态匹配系统 CJK 字体；流式文档原生支持轻小说振假名（Ruby）、学术 LaTeX 公式（行内/独行）及代码高亮与行号视图。
- **多标签与节能休眠**：支持多文档标签页切换，左右箭头导航与自动定位当前标签；后台标签页闲置 5 分钟或超出最近 5 个时自动进入节能休眠释放内存，重启时仅加载当前标签页，其余标签按需唤醒。
- **灵活阅读体验**：支持浅色/深色主题、PDF/Word 深色智能反色（保留鲜亮色彩与插图，扫描 PDF 整页反色）、流式正文左右边缘拖拽调整阅读宽度、自动侧边栏（有目录或批注时自动展开）及 `F11` 专注模式。
- **本地离线与便捷安装**：阅读进度、书签和设置全部保存在本机；单个 EXE 支持按用户安装、文件格式关联、开始菜单/桌面快捷方式及从 GitHub 检查和安装更新。

### 常用快捷键

- `Ctrl+O` 打开文件 · `Ctrl+F` 搜索正文 · `Ctrl+B` 锁定/自动侧边栏
- `Ctrl+W` 关闭当前标签 · `Ctrl+Tab` 切换标签 · `F11` 专注模式
- `Ctrl+滚轮` / `Ctrl+±` / `Ctrl+0` 缩放与重置 · `Ctrl+A` 全选正文 · `Ctrl+C` 复制

### 注意事项

复杂的 Word 排版可能存在显示差异。受 DRM 保护或采用 HUFF/CDIC 压缩的 Kindle 文件暂不可读。OCR 与 Markdown 编辑暂列后续版本。

GitHub 仓库 [github.com/drliuhuan/HayakuReader](https://github.com/drliuhuan/HayakuReader)

© 2026 drliuhuan · Apache-2.0
