# Extracted verbatim from release.yml's 'Install LLVM 22 with conda (Windows)' step so both the
# release build and the PR smoke matrix use one copy.

conda config --set remote_connect_timeout_secs 60
conda config --set remote_read_timeout_secs 300
conda config --set remote_max_retries 10
conda config --set remote_backoff_factor 2

$solver = if (Get-Command mamba -ErrorAction SilentlyContinue) { "mamba" } else { "conda" }
$createArgsFast = "create -y -n mux-llvm -c conda-forge --repodata-fn current_repodata.json llvmdev=22 clang=22 zlib zstd libxml2"
$createArgsFull = "create -y -n mux-llvm -c conda-forge llvmdev=22 clang=22 zlib zstd libxml2"
$maxAttempts = 5
$attempt = 1

while ($true) {
  Write-Host "[$attempt/$maxAttempts] Installing LLVM toolchain with $solver (current_repodata)"
  cmd /c "$solver $createArgsFast"
  if ($LASTEXITCODE -eq 0) {
    break
  }

  Write-Host "Fast repodata attempt failed, trying full repodata"
  cmd /c "$solver $createArgsFull"
  if ($LASTEXITCODE -eq 0) {
    break
  }

  if ($attempt -ge $maxAttempts) {
    throw "Failed to install conda packages after $maxAttempts attempts"
  }

  $sleepSeconds = 10 * $attempt
  Write-Host "Package install failed. Retrying in $sleepSeconds seconds..."
  Start-Sleep -Seconds $sleepSeconds
  $attempt += 1
}

$condaBase = & conda info --base
$envRoot = Join-Path $condaBase "envs\mux-llvm"
$llvmPrefix = Join-Path $envRoot "Library"
$llvmBin = Join-Path $llvmPrefix "bin"
$llvmLib = Join-Path $llvmPrefix "lib"
$llvmInclude = Join-Path $llvmPrefix "include"
$llvmConfigPath = Join-Path $llvmBin "llvm-config.exe"
$clangPath = Join-Path $llvmBin "clang.exe"
$clangxxPath = Join-Path $llvmBin "clang++.exe"
$clangClPath = Join-Path $llvmBin "clang-cl.exe"

if (-not (Test-Path $llvmConfigPath)) {
  throw "llvm-config.exe not found at $llvmConfigPath"
}

if (-not (Test-Path $clangPath)) {
  throw "clang.exe not found in $llvmBin"
}

if (-not (Test-Path $clangxxPath)) {
  if (Test-Path $clangClPath) {
    $clangxxPath = $clangClPath
  } else {
    $clangxxPath = $clangPath
  }
}

$llvmVersion = & $llvmConfigPath --version
if (-not $llvmVersion.StartsWith("22.")) {
  throw "Expected LLVM 22.x on Windows runner, found $llvmVersion at $llvmConfigPath"
}

$requiredTokens = @()
foreach ($args in @(@("--system-libs", "--link-static"), @("--libs", "--link-static"))) {
  $out = & $llvmConfigPath @args
  if ($LASTEXITCODE -ne 0) {
    throw "llvm-config $($args -join ' ') failed"
  }
  $requiredTokens += ($out -split "\s+")
}

$requiredLibNames = New-Object System.Collections.Generic.HashSet[string]
foreach ($tok in $requiredTokens) {
  if ([string]::IsNullOrWhiteSpace($tok)) {
    continue
  }
  $name = $tok.Trim('"')
  if ($name.StartsWith("-l")) {
    $name = $name.Substring(2) + ".lib"
  } elseif ($name.StartsWith("/DEFAULTLIB:")) {
    $name = $name.Substring(12)
  }
  if ($name -match "\.lib$") {
    [void]$requiredLibNames.Add($name)
  }
}

$aliases = @{
  "z.lib" = @("zlib.lib", "zlibstatic.lib", "zdll.lib")
  "zstd.dll.lib" = @("zstd.lib", "zstd_static.lib", "libzstd.lib")
  "xml2.lib" = @("libxml2.lib", "libxml2.dll.lib", "libxml2_a.lib")
}

foreach ($libName in $requiredLibNames) {
  $baseName = if ([System.IO.Path]::IsPathRooted($libName)) { [System.IO.Path]::GetFileName($libName) } else { $libName }
  $targetPath = Join-Path $llvmLib $baseName
  if (Test-Path $targetPath) {
    continue
  }

  $candidateNames = New-Object System.Collections.Generic.List[string]
  if ($aliases.ContainsKey($baseName)) {
    foreach ($n in $aliases[$baseName]) {
      [void]$candidateNames.Add($n)
    }
  }
  if ($baseName.EndsWith(".dll.lib")) {
    [void]$candidateNames.Add($baseName.Replace(".dll.lib", ".lib"))
  }
  if ($baseName.StartsWith("lib") -and $baseName.EndsWith(".lib")) {
    [void]$candidateNames.Add($baseName.Substring(3))
  } else {
    [void]$candidateNames.Add("lib$baseName")
  }

  $sourcePath = $null
  foreach ($candidate in $candidateNames) {
    $p = Join-Path $llvmLib $candidate
    if (Test-Path $p) {
      $sourcePath = $p
      break
    }
  }

  if ($sourcePath) {
    Copy-Item $sourcePath $targetPath
    Write-Host "Created $baseName shim from $(Split-Path -Leaf $sourcePath)"
  } elseif ($baseName -eq "xml2.lib") {
    $xmlCandidate = Get-ChildItem -Path $llvmLib -Filter "*xml2*" -File -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($xmlCandidate) {
      Copy-Item $xmlCandidate.FullName $targetPath
      Write-Host "Created $baseName shim from $($xmlCandidate.Name) (fallback glob)"
    } else {
      Write-Warning "Could not create xml2.lib shim; no xml2 lib found in $llvmLib"
    }
  }
}

$systemLibAllowlist = @(
  "psapi.lib", "shell32.lib", "ole32.lib", "uuid.lib", "advapi32.lib",
  "kernel32.lib", "ntdll.lib", "userenv.lib", "ws2_32.lib", "dbghelp.lib",
  "legacy_stdio_definitions.lib"
)

$missingLibs = @()
foreach ($libName in $requiredLibNames) {
  $baseName = if ([System.IO.Path]::IsPathRooted($libName)) { [System.IO.Path]::GetFileName($libName) } else { $libName }
  if ($systemLibAllowlist -contains $baseName.ToLowerInvariant()) {
    continue
  }

  if ([System.IO.Path]::IsPathRooted($libName)) {
    if (-not (Test-Path $libName)) {
      $missingLibs += $libName
    }
    continue
  }

  if (-not (Test-Path (Join-Path $llvmLib $baseName))) {
    $missingLibs += $libName
  }
}

if ($missingLibs.Count -gt 0) {
  Write-Host "Warning: unresolved libraries after shim pass (link may still succeed):"
  $missingLibs | ForEach-Object { Write-Host "  - $_" }
  Write-Host "Available .lib files in ${llvmLib}:"
  Get-ChildItem -Path $llvmLib -Filter "*.lib" -File -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name
}

# Kept in step with the Linux and macOS branches of action.yml. Without it the
# exact-version check is off, and conda pins only the major (llvmdev=22) - so a
# move to 22.2 would silently link a mismatched LLVM into a shipped artifact
# instead of failing the build.
"LLVM_SYS_221_STRICT_VERSIONING=1" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
"LLVM_SYS_221_PREFIX=$llvmPrefix" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
"LLVM_CONFIG_PATH=$llvmConfigPath" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
"LIB=$llvmLib;$env:LIB" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
$llvmBin | Out-File -FilePath $env:GITHUB_PATH -Encoding utf8 -Append
