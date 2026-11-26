param(
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
Push-Location $project
try {
    $flags = @('--locked', '--release', '--target', 'x86_64-pc-windows-msvc')
    if ($Offline) { $flags += '--offline' }
    $messages = cargo build @flags --message-format=json-render-diagnostics |
        ForEach-Object { $_ | ConvertFrom-Json }
    if ($LASTEXITCODE -ne 0) { throw 'Cargo build failed.' }
    $app = $messages | Where-Object {
        $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'hayaku-reader' -and $_.executable
    } | Select-Object -Last 1
    if (-not $app) { throw 'Cargo did not report the application executable.' }

    $destination = Join-Path $project 'dist'
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    $exe = Join-Path $destination 'HayakuReader.exe'
    Copy-Item -LiteralPath $app.executable -Destination $exe -Force

    $pdfiumCandidates = @()
    if ($env:PDFIUM_DLL) { $pdfiumCandidates += $env:PDFIUM_DLL }
    if ($env:USERPROFILE) {
        $pdfiumCandidates += (Join-Path $env:USERPROFILE '.cache\pdfium-bundled\pdfium-8066\pdfium.dll')
    }
    if ($env:LOCALAPPDATA) {
        $pdfiumCandidates += (Join-Path $env:LOCALAPPDATA 'HayakuReader\pdfium\pdfium-8066\pdfium.dll')
    }
    $pdfiumSrc = $pdfiumCandidates | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
    if ($pdfiumSrc) {
        $pdfiumDst = Join-Path $destination 'pdfium.dll'
        Copy-Item -LiteralPath $pdfiumSrc -Destination $pdfiumDst -Force
        Write-Output $pdfiumDst
    }
    Write-Output $exe
} finally {
    Pop-Location
}
