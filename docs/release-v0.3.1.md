# HayakuReader v0.3.1

Windows 原生多格式阅读器正式版。

- 使用 egui / wgpu 原生 GPU 渲染，无需 WebView2；PDFium 内嵌于单文件 EXE。
- 支持 PDF、EPUB、TXT、Kindle、Word、HTML、XML 和 Markdown，提供多标签、搜索、批注、深色阅读与后台标签休眠。
- 修复正文滚动条：点击轨道空白处按页滚动，保留滑块拖动。
- 修复切换文档时的阅读位置恢复及滚动状态串用。
- 修复 EPUB SVG 插图引用，加载元数据中声明的封面并改善图片高度计算。
- 修复 EPUB 目录跳转后高亮上一条的问题。
- 默认优先低功耗 GPU；硬件 Vulkan 可用时避免 DX12 枚举持续唤醒独显。

## 下载

下载唯一附件 `HayakuReader.exe`，双击运行。支持按用户安装及文件关联。

复杂 Word 排版可能存在显示差异；受 DRM 保护或 HUFF/CDIC 压缩的 Kindle 文件暂不支持。
