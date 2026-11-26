# HayakuReader

HayakuReader 是 **drliuhuan** 用 Rust 构建的 Windows 本地阅读器。可以打开 PDF、EPUB、TXT、AZW3、AZW、MOBI、DOC、DOCX、HTML 和 Markdown，复制文字、做高亮和笔记、添加书签。

- 用标签页在文档之间切换。离开较久或较早打开的标签页会休眠，再次切回即可继续阅读。
- 阅读进度和批注保存在本机。启动时可以恢复上次打开的标签页。
- 单个 EXE 支持按用户安装。安装后可选择与 HayakuReader 关联的文件格式，并从 GitHub 检查和安装更新。

v0.2.1 将 HTM 和 HTML 改为默认不关联。你仍可在安装时或安装后的设置中选择关联这两种格式。

v0.2.2 显示更新下载进度和速度。更新失败时可转到 GitHub 手动下载。旧进程阻止更新时，应用会提示关闭旧版后重试。

v0.2.4 安装时会在开始菜单和桌面自动创建快捷方式；通过关联文件启动时，启动画面在文件加载完成后即刻消失。新增全局快捷键（`Ctrl+O/F/B/W/Tab`、`Ctrl+滚轮` 缩放、`F11` 专注模式）、悬浮设置与搜索条（支持上下匹配跳转与计数）、四色一键高亮与正文点击编辑、深色模式智能页面反色（保留鲜亮色彩与插图，提亮暗彩色并支持扫描 PDF 整页反色）、Markdown / HTML 渲染与代码模式切换（带行号与语法配色）、批注导出为 Markdown、起始页最近阅读直达与记录删除/清空、大 PDF 分块加载，以及 EPUB/Kindle 多级目录与页内锚点支持。

v0.2.5 进一步优化性能与内存占用：为 PDF/Word 页面增加 LRU 显存回收与合理像素预算、优化滚动与批注高亮热路径、采用零分配智能反色 LUT 与零拷贝静态资源响应，并启用 Thin LTO 减小单文件体积。

复杂的 Word 排版可能存在显示差异。受 DRM 保护或采用 HUFF/CDIC 压缩的 Kindle 文件暂不可读。OCR 与 Markdown 编辑暂列后续版本。

GitHub 仓库 [github.com/drliuhuan/HayakuReader](https://github.com/drliuhuan/HayakuReader)

© 2026 drliuhuan · Apache-2.0
