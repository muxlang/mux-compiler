# Extracted verbatim from release.yml's 'Validate LLVM toolchain (Windows)' step so both the
# release build and the PR smoke matrix use one copy.

if (-not $env:LLVM_CONFIG_PATH) {
  throw "LLVM_CONFIG_PATH is not set"
}
if (-not $env:LLVM_SYS_221_PREFIX) {
  throw "LLVM_SYS_221_PREFIX is not set"
}
if (-not (Test-Path $env:LLVM_SYS_221_PREFIX)) {
  throw "LLVM_SYS_221_PREFIX path does not exist: $env:LLVM_SYS_221_PREFIX"
}
if (-not (Test-Path (Join-Path $env:LLVM_SYS_221_PREFIX "bin\llvm-config.exe"))) {
  throw "Expected llvm-config at LLVM_SYS_221_PREFIX\\bin\\llvm-config.exe"
}
& $env:LLVM_CONFIG_PATH --version
& (Join-Path $env:LLVM_SYS_221_PREFIX "bin\llvm-config.exe") --version
$cl = Get-Command cl -ErrorAction SilentlyContinue
if (-not $cl) {
  throw "MSVC compiler (cl.exe) not found on PATH"
}
