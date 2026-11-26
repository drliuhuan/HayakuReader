param(
    [switch]$SkipAssets,
    [switch]$Offline
)

$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
Push-Location $project
try {
    if (-not $SkipAssets) {
        npm.cmd ci --ignore-scripts --omit=optional
        if ($LASTEXITCODE -ne 0) { throw 'Could not install front-end assets.' }
    }

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
    Copy-Item -LiteralPath $app.executable -Destination $exe
    Write-Output $exe
} finally {
    Pop-Location
}
