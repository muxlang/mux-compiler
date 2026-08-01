# Extracted verbatim from release.yml's 'Create xml2 import lib (Windows)' step so both the
# release build and the PR smoke matrix use one copy.

$libDir = "$env:LLVM_SYS_221_PREFIX\lib"
$binDir = "$env:LLVM_SYS_221_PREFIX\bin"
$outLib = Join-Path $libDir "xml2.lib"

if (Test-Path $outLib) {
  Write-Host "xml2.lib already exists, skipping"
  exit 0
}

$xml2Dll = Get-ChildItem -Path $binDir -File -Filter "libxml2*.dll" | Select-Object -First 1
if (-not $xml2Dll) {
  Write-Host "No libxml2*.dll in $binDir, listing DLLs:"
  Get-ChildItem -Path $binDir -File -Filter "*.dll" | Select-Object -ExpandProperty Name | Sort-Object
  throw "Cannot create xml2.lib: no libxml2 DLL found"
}
Write-Host "Building xml2.lib from: $($xml2Dll.FullName)"

$rawExports = & dumpbin /exports $xml2Dll.FullName
$defLines = @("LIBRARY `"$($xml2Dll.BaseName)`"", "EXPORTS")
$inTable = $false
foreach ($line in $rawExports) {
  if ($line -match "ordinal\s+hint\s+RVA\s+name") { $inTable = $true; continue }
  if ($inTable -and $line -match "^\s+\d+\s+[\da-fA-F]+\s+[\da-fA-F]+\s+(\S+)") {
    $defLines += $Matches[1]
  }
}
if ($defLines.Count -le 2) {
  throw "Failed to parse any exports from $($xml2Dll.FullName)"
}
Write-Host "Parsed $($defLines.Count - 2) exports"

$defPath = Join-Path $env:TEMP "libxml2.def"
($defLines -join "`r`n") | Out-File -FilePath $defPath -Encoding ascii

& lib /nologo /machine:x64 /def:$defPath /out:$outLib
if ($LASTEXITCODE -ne 0) { throw "lib.exe failed to create xml2.lib" }
Write-Host "Created xml2.lib ($((Get-Item $outLib).Length) bytes)"
