<#
.SYNOPSIS
    ローカル環境でビルド・署名・リリースを一括実行する。

.DESCRIPTION
    以下の 3 ステップを順次実行する:
        1. cargo build --release で exe および aux2 を生成
        2. signtool で物理トークンを用いたコード署名と検証
        3. git tag 作成と gh release によるアセットアップロード

    物理ハードウェアトークンが接続されていない場合、ステップ 2 で失敗する。

.PARAMETER Version
    リリースバージョン文字列 (例: "0.1.0")。
    git tag には "v" プレフィックスが自動付与される。

.EXAMPLE
    .\scripts\release.ps1 -Version "0.1.0"

.NOTES
    前提条件: cargo, signtool, gh CLI がパス上に存在すること。
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$Version
)

$ErrorActionPreference = "Stop"

# Helper function to check for command existence
function Assert-CommandExists {
    param($command)
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "Required command '$command' not found in PATH. Please install it and try again."
    }
}

function Assert-LastExitCode {
    param([string]$Action)

    if ($LASTEXITCODE -ne 0) {
        throw "$Action failed with exit code $LASTEXITCODE."
    }
}

try {
    Write-Host "=== 0/5: Check Prerequisites ==="
    "cargo", "signtool", "git", "gh" | ForEach-Object { Assert-CommandExists $_ }
    Write-Host "All prerequisites found."

    $repoRoot = Split-Path -Parent $PSScriptRoot

    Write-Host "=== 1/5: Build ==="

    cargo build --release --manifest-path (Join-Path $repoRoot "Cargo.toml")
    Assert-LastExitCode "cargo build --release"

    Write-Host "=== 2/5: Sign ==="

    $releaseDir = Join-Path $repoRoot "target\release"
    $exePath = Join-Path $releaseDir "audio_rec_cli.exe"
    $dllPath = Join-Path $releaseDir "aviutl2_audio_rec.dll"

    # dll は cdylib のビルド成果物であり PE バイナリのため signtool で署名可能
    # リリース ZIP に同梱する際に .aux2 にリネームする
    signtool sign /tr http://timestamp.digicert.com /td sha256 /fd sha256 /a $exePath
    Assert-LastExitCode "signtool sign (audio_rec_cli.exe)"
    signtool sign /tr http://timestamp.digicert.com /td sha256 /fd sha256 /a $dllPath
    Assert-LastExitCode "signtool sign (aviutl2_audio_rec.dll)"

    signtool verify /pa $exePath
    Assert-LastExitCode "signtool verify (audio_rec_cli.exe)"
    signtool verify /pa $dllPath
    Assert-LastExitCode "signtool verify (aviutl2_audio_rec.dll)"

    Write-Host "=== 3/5: Zip Artifacts ==="

    $distDir = Join-Path $repoRoot "dist"
    $stagingDir = Join-Path $distDir "release"
    $packageRoot = Join-Path $stagingDir "Plugin\Beive60"
    $zipFileName = "aviutl2-audio-rec-v$($Version).au2pkg.zip"
    $zipPath = Join-Path $distDir $zipFileName

    # Clean and create staging directory
    if (Test-Path $stagingDir) { Remove-Item -Recurse -Force $stagingDir }
    New-Item -ItemType Directory -Path $packageRoot -Force | Out-Null

    # Copy files to the staging directory
    Copy-Item -Path $exePath -Destination $stagingDir
    Copy-Item -Path $dllPath -Destination (Join-Path $packageRoot "aviutl2_audio_rec.aux2")
    Copy-Item -Path (Join-Path $repoRoot "README.md") -Destination $stagingDir
    Copy-Item -Path (Join-Path $repoRoot "LICENSE") -Destination $stagingDir

    # Create the archive from the staging directory's contents
    Compress-Archive -Path "$stagingDir\*" -DestinationPath $zipPath -Force

    # Clean up the staging directory
    Remove-Item -Recurse -Force $stagingDir

    Write-Host "Created release zip: $zipPath"

    Write-Host "=== 4/5: Release ==="

    Write-Host "Checking GitHub CLI authentication status..."
    gh auth status
    if ($LASTEXITCODE -ne 0) {
        throw "GitHub CLI not authenticated. Please run 'gh auth login' and try again."
    }
    Write-Host "GitHub CLI is authenticated."

    git -C $repoRoot tag "v$Version"
    Assert-LastExitCode "git tag"

    git -C $repoRoot push origin "v$Version"
    Assert-LastExitCode "git push origin v$Version"

    gh release create "v$Version" $zipPath --repo beive60/aviutl2-audio-rec --title "Release v$Version" --generate-notes
    Assert-LastExitCode "gh release create"

    Write-Host "=== 5/5: Done ==="
    Write-Host "Released v$Version successfully."
}
catch {
    Write-Host "`nError during release process:"
    Write-Host "  - $($_.Exception.Message)"
    exit 1
}
