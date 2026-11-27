# HayakuReader 工作区工程规则

## 开发与打包交付流程
- 每次修改代码后，都必须完成测试、静态检查并执行本地打包生成 `dist\HayakuReader.exe`，以便用户直接运行测试。
- 非安装目录运行 `alpha` 版本时，`src\platform.rs` 的 `check_manual_update` 会无视版本号是否变化强制覆盖更新用户个人安装目录（`%LOCALAPPDATA%\Programs\HayakuReader\HayakuReader.exe`）。
- **未经过用户实测确认前**：不得擅自迭代版本号，不得执行 `git commit` / `git push`，也不得发布 GitHub Pre-release。
- 仅在用户确认当前本地包无问题并要求迭代/发版后，再更新版本号、提交推送并创建 Pre-release。

## 本地自动化实测安全隔离
- 若需要在开发机上自动化启动 `dist\HayakuReader.exe` 或 `target\debug\hayaku-reader.exe` 做窗口/显卡等实机探测，必须先将子进程的 `LOCALAPPDATA` 临时重定向到新建的隔离目录并在结束后清理恢复，禁止在未隔离的真实 `LOCALAPPDATA` 下直接启动未发布构建，以免误覆盖用户本机安装实例。

## Windows 构建与图形后端约束
- 在本机 PowerShell 中调用 `cargo`、打包或检查命令前，需前置 `$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"`。
- 每次修改后的标准验证与打包命令顺序为：
  1. `cargo test --locked`
  2. `cargo clippy --locked --all-targets -- -D warnings`
  3. `.\scripts\package-windows.ps1`
- 保持 `src\main.rs` 中 `default_wgpu_options()` 在未显式设置 `WGPU_BACKEND` 且存在硬件 Vulkan 适配器时默认使用 `Backends::VULKAN` + `PowerPreference::LowPower`（仍保留环境变量覆盖），避免 `egui-wgpu` 常驻 `available_adapters` 时因 DX12 枚举在双显卡笔记本上持续持有独显 `ID3D12Device` 而引起切换延迟与最大化卡顿。
