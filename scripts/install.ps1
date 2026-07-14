$ErrorActionPreference = "Stop"

$Repo = "muxlang/mux-compiler"
$InstallDir = if ($env:MUX_INSTALL_DIR) { $env:MUX_INSTALL_DIR } else { Join-Path $env:USERPROFILE ".mux\bin" }
$LibDir = if ($env:MUX_LIB_DIR) { $env:MUX_LIB_DIR } else { Join-Path (Split-Path -Parent $InstallDir) "lib" }
$BaseUrl = if ($env:MUX_RELEASE_BASE_URL) { $env:MUX_RELEASE_BASE_URL } else { "https://github.com/$Repo/releases/latest/download" }

if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}

if (-not (Test-Path $LibDir)) {
    New-Item -ItemType Directory -Path $LibDir -Force | Out-Null
}

$arch = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
$target = if ($arch -eq "x86_64") { "windows-x86_64" } else { "windows-$arch" }
$archive = "mux-$target.zip"
$archiveUrl = "$BaseUrl/$archive"
$checksumUrl = "$archiveUrl.sha256"

$tmpDir = Join-Path $env:TEMP ("mux-install-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null

try {
    $archivePath = Join-Path $tmpDir $archive
    $checksumPath = Join-Path $tmpDir "$archive.sha256"

    Write-Host "Downloading $archive"
    Invoke-WebRequest -Uri $archiveUrl -OutFile $archivePath
    Invoke-WebRequest -Uri $checksumUrl -OutFile $checksumPath

    $expected = (Get-Content $checksumPath).Split(" ")[0].Trim().ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 $archivePath).Hash.ToLower()
    if ($expected -ne $actual) {
        throw "Checksum verification failed"
    }

    Expand-Archive -Path $archivePath -DestinationPath $tmpDir -Force
    $bundleRoot = Join-Path $tmpDir "mux-$target"
    $muxExe = Join-Path $bundleRoot "bin/mux.exe"

    if (-not (Test-Path $muxExe)) {
        throw "Could not find mux.exe in archive"
    }

    Copy-Item $muxExe (Join-Path $InstallDir "mux.exe") -Force

    $bundleLibDir = Join-Path $bundleRoot "lib"
    if (Test-Path $bundleLibDir) {
        Get-ChildItem -Path $bundleLibDir -File | ForEach-Object {
            Copy-Item $_.FullName (Join-Path $LibDir $_.Name) -Force

            if ($_.Name -match "\.dll$") {
                Copy-Item $_.FullName (Join-Path $InstallDir $_.Name) -Force
            }
        }
    }

    Write-Host "Installed mux to $(Join-Path $InstallDir "mux.exe")"
    Write-Host "Installed runtime libraries to $LibDir"

    $currentUserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($currentUserPath -notlike "*$InstallDir*") {
        [Environment]::SetEnvironmentVariable("Path", "$currentUserPath;$InstallDir", "User")
        Write-Host "Added $InstallDir to user PATH. Restart your shell to use mux."
    }

    & (Join-Path $InstallDir "mux.exe") version
}
finally {
    if (Test-Path $tmpDir) {
        Remove-Item -Path $tmpDir -Recurse -Force
    }
}
