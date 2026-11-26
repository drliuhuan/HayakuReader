param(
    [string]$Target,
    [switch]$SkipAssets,
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
Push-Location $project

try {
    if (-not $Target) {
        $hostLine = rustc -vV | Where-Object { $_ -like 'host: *' }
        if ($LASTEXITCODE -ne 0 -or -not $hostLine) { throw 'Rust toolchain unavailable.' }
        $Target = $hostLine.Substring(6)
    }
    if ($Target -notmatch '^(x86_64|i686|aarch64)-pc-windows-(gnu|msvc)$') {
        throw "Unsupported Windows build target: $Target"
    }
    $architecture = @{ x86_64 = 'x64'; i686 = 'x86'; aarch64 = 'arm64' }[$Matches[1]]
    $usesGnu = $Matches[2] -eq 'gnu'

    if (-not $SkipAssets) {
        npm.cmd ci --ignore-scripts --omit=optional
        if ($LASTEXITCODE -ne 0) { throw 'Could not install front-end assets.' }
    }

    $cargoFlags = @('--locked')
    if ($Offline) { $cargoFlags += '--offline' }
    $messages = cargo build @cargoFlags --release --target $Target --message-format=json-render-diagnostics |
        ForEach-Object { $_ | ConvertFrom-Json }
    if ($LASTEXITCODE -ne 0) { throw 'Cargo build failed.' }

    $app = $messages | Where-Object {
        $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'hayaku-reader' -and $_.executable
    } | Select-Object -Last 1
    if (-not $app) { throw 'Cargo did not report the application executable.' }

    $destination = Join-Path $project "dist\HayakuReader-$Target"
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    Copy-Item -LiteralPath $app.executable -Destination (Join-Path $destination 'HayakuReader.exe')
    if ($usesGnu) {
        $loader = $messages | Where-Object {
            $_.reason -eq 'build-script-executed' -and $_.package_id -match 'webview2-com-sys'
        } | Select-Object -Last 1
        if (-not $loader) { throw 'Cargo did not report the WebView2 loader.' }
        $dll = Join-Path $loader.out_dir "$architecture\WebView2Loader.dll"
        Copy-Item -LiteralPath $dll -Destination $destination
    }
    Copy-Item -LiteralPath 'README.md' -Destination $destination
    Copy-Item -LiteralPath 'HayakuReader-需求文档.md' -Destination $destination
    $licenses = Join-Path $destination 'licenses'
    New-Item -ItemType Directory -Path $licenses -Force | Out-Null
    Copy-Item -LiteralPath 'node_modules\pdfjs-dist\LICENSE' -Destination (Join-Path $licenses 'PDF.js-LICENSE.txt')
    Copy-Item -LiteralPath 'node_modules\dompurify\LICENSE' -Destination (Join-Path $licenses 'DOMPurify-LICENSE.txt')
    foreach ($assetGroup in 'cmaps', 'iccs', 'standard_fonts', 'wasm') {
        Get-ChildItem -LiteralPath "node_modules\pdfjs-dist\$assetGroup" -Filter 'LICENSE*' -File |
            ForEach-Object {
                Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $licenses ("PDF.js-$assetGroup-" + $_.Name + '.txt'))
            }
    }
    Write-Output $destination
} finally {
    Pop-Location
}
