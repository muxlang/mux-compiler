$ErrorActionPreference = "Stop"

if ($args.Count -eq 0) {
    Write-Host "Usage: .\scripts\dev-cargo.ps1 <cargo args...>"
    exit 1
}

$llvmConfig = $null

if (Get-Command llvm-config-22 -ErrorAction SilentlyContinue) {
    $llvmConfig = (Get-Command llvm-config-22).Source
} elseif (Test-Path "C:\Program Files\LLVM\bin\llvm-config.exe") {
    $llvmConfig = "C:\Program Files\LLVM\bin\llvm-config.exe"
} else {
    Write-Host "Could not find llvm-config-22. Run scripts/bootstrap-dev.ps1 first."
    exit 1
}

$llvmPrefix = & $llvmConfig --prefix
$env:LLVM_CONFIG_PATH = $llvmConfig
$env:LLVM_SYS_221_PREFIX = $llvmPrefix

if (Get-Command clang-22 -ErrorAction SilentlyContinue) {
    $env:CC = (Get-Command clang-22).Source
}

if (Get-Command clang++-22 -ErrorAction SilentlyContinue) {
    $env:CXX = (Get-Command clang++-22).Source
}

& cargo @args
exit $LASTEXITCODE
