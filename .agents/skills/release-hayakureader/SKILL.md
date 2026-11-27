---
name: release-hayakureader
description: >-
  Use this skill only after the user confirms local test results and asks to bump the version,
  commit, push, and publish a GitHub Pre-release for HayakuReader on Windows.
---

# HayakuReader 版本迭代与 Windows Pre-release 发布流程

仅在用户测试通过并明确指示“迭代版本 / 提交推送 / 发包发布”时执行本流程。

## 步骤

1. **同步迭代版本号**
   - 同步更新根目录 `Cargo.toml` 的 `version` 与 `Cargo.lock` 中 `[[package]] name = "hayaku-reader"` 下的 `version`（例如 `0.3.0-alpha.7` -> `0.3.0-alpha.8`），保证 `--locked` 构建可通过。

2. **清理干扰环境变量并执行全量验证与打包**
   - 在同一个 PowerShell 会话中前置 Cargo 路径并清理 `WGPU_*` 变量：
     ```powershell
     $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
     Remove-Item Env:WGPU_POWER_PREF, Env:WGPU_BACKEND -ErrorAction SilentlyContinue
     cargo test --locked
     if ($LASTEXITCODE -eq 0) { cargo clippy --locked --all-targets -- -D warnings }
     if ($LASTEXITCODE -eq 0) { .\scripts\package-windows.ps1 }
     ```
   - 校验并记录 `dist\HayakuReader.exe` 的文件字节大小与 SHA-256（`(Get-FileHash .\dist\HayakuReader.exe -Algorithm SHA256).Hash`）。

3. **隔离实机冒烟校验（可选但推荐，针对窗口/渲染/平台改动必做）**
   - 临时将 `LOCALAPPDATA` 指向空临时目录启动 `dist\HayakuReader.exe`（避免触发 `alpha` 对本机安装目录的强制覆盖更新），确认进程正常起窗未闪退且双显卡本下 `nvidia-smi` 无异常绑定，随后关闭进程并清理临时目录与恢复 `LOCALAPPDATA`。

4. **提交并推送至 `origin v2-native`**
   - 用 `git status -s` 核对仅包含预期改动文件，按逻辑精确 `git add` 后提交，并执行 `git push origin v2-native`。

5. **创建并校验 GitHub Pre-release**
   - 先获取完整的 40 位 Commit SHA（短 SHA 会导致 `gh release create --target` 返回 HTTP 422）：
     ```powershell
     $sha = (git rev-parse HEAD).Trim()
     ```
   - 使用单引号 Here-String（`@'...'@`）将 Release Notes 写入 UTF-8 临时文件并用 `--notes-file` 传入，防止 PowerShell 字符串插值吞掉反引号：
     ```powershell
     $notesFile = Join-Path $env:TEMP "hayaku-release-notes.md"
     [System.IO.File]::WriteAllText($notesFile, $notes)
     try {
       gh release create "v<version>" "dist\HayakuReader.exe#HayakuReader.exe" `
         --target $sha `
         --prerelease `
         --title "HayakuReader v<version>" `
         --notes-file $notesFile
     } finally {
       Remove-Item $notesFile -ErrorAction SilentlyContinue
     }
     ```
   - 最后执行 `gh release view "v<version>" --json tagName,url,isPrerelease,assets` 确认附件状态为 `uploaded` 且 SHA-256 摘要与本地构建一致。
