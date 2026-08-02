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

    # Everything in bin/, not just mux.exe: the bundle also ships the LLVM and
    # support DLLs (LLVM-C.dll, libxml2.dll, zlib.dll, zstd.dll) that mux.exe
    # loads at run time, and Windows resolves those from the executable's own
    # directory. Copying only the exe produced a missing-DLL failure on first
    # use.
    Get-ChildItem -Path (Join-Path $bundleRoot "bin") -File | ForEach-Object {
        Copy-Item $_.FullName (Join-Path $InstallDir $_.Name) -Force
    }

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

    # Downloading the archive is not the same as being able to compile: the
    # compiler shells out to a matching clang to link every program. `mux
    # doctor` checks that and prints the install command for whatever is
    # missing, so a gap surfaces here instead of as a linker error on the
    # user's first program.
    Write-Host ""
    # A failing doctor is reported, not fatal - the install itself succeeded.
    # PowerShell 7.4+ turns a nonzero native exit code into a terminating error
    # under `$ErrorActionPreference = "Stop"`, so opt out for this one call.
    # Assigning the variable is harmless on Windows PowerShell, where it does
    # not exist.
    $previousNativeErrorPreference = $PSNativeCommandUseErrorActionPreference
    try {
        $PSNativeCommandUseErrorActionPreference = $false
        & (Join-Path $InstallDir "mux.exe") doctor
    }
    finally {
        $PSNativeCommandUseErrorActionPreference = $previousNativeErrorPreference
    }
    if ($LASTEXITCODE -ne 0) {
        Write-Host ""
        Write-Host "mux is installed at $(Join-Path $InstallDir "mux.exe"), but the checks above did not pass."
        Write-Host "Install the missing dependencies, then re-run: mux doctor"
    }
}
finally {
    if (Test-Path $tmpDir) {
        Remove-Item -Path $tmpDir -Recurse -Force
    }
}
