$ErrorActionPreference = "Stop"

if (-not (Get-Command choco -ErrorAction SilentlyContinue)) {
    Write-Host "Chocolatey is required on Windows to run this bootstrap script."
    Write-Host "Install from https://chocolatey.org/install and rerun this script."
    exit 1
}

Write-Host "Installing LLVM 22 and clang toolchain"
choco install llvm --version=22.1.0 -y --no-progress

Write-Host "Installed contributor dependencies."

Write-Host "Configuring git hooks..."
git config core.hooksPath .github/hooks

Write-Host "Git hooks configured."
Write-Host "Use .\scripts\dev-cargo.ps1 for source builds without manual env vars."
Write-Host "Example: .\scripts\dev-cargo.ps1 build"
