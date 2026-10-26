# Build the release exe and package it as dist\Cyclops-<version>-setup.exe (Inno Setup 6).
#   powershell -File installer\build.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
& "$env:USERPROFILE\.cargo\bin\cargo.exe" build --release
if ($LASTEXITCODE) { throw "cargo build failed" }

# Stage everything the installer ships.
$pkg = Join-Path $root 'target\package'
Remove-Item -Recurse -Force $pkg -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$pkg\licenses" | Out-Null
Copy-Item target\release\cyclops.exe $pkg

# Visual C++ runtime (app-local deployment): the C++ runtime plus OpenMP for LibRaw.
$redist = Get-ChildItem "${env:ProgramFiles}\Microsoft Visual Studio\*\*\VC\Redist\MSVC\*\x64" -Directory |
    Sort-Object FullName -Descending | Select-Object -First 1
if (-not $redist) { throw "Visual C++ redistributable files not found" }
foreach ($dll in 'Microsoft.VC143.CRT\msvcp140.dll', 'Microsoft.VC143.CRT\vcruntime140.dll',
                 'Microsoft.VC143.CRT\vcruntime140_1.dll', 'Microsoft.VC143.OpenMP\vcomp140.dll') {
    Copy-Item (Join-Path $redist.FullName $dll) $pkg
}

# Licences: Cyclops's own and every bundled third-party component's.
$licenses = @{
    'Cyclops-LICENSE-MIT.txt'         = 'LICENSE-MIT'
    'Cyclops-LICENSE-APACHE.txt'      = 'LICENSE-APACHE'
    'THIRD-PARTY-NOTICES.md'          = 'third_party\README.md'
    'LibRaw-LICENSE.CDDL.txt'         = 'third_party\libraw\LICENSE.CDDL'
    'LibRaw-COPYRIGHT.txt'            = 'third_party\libraw\COPYRIGHT'
    'GPR-SDK-LICENSE.txt'             = 'third_party\gpr\LICENSE.txt'
    'Adobe-DNG-SDK-BSD-License.txt'   = 'third_party\gpr\source\lib\dng_sdk\BSD-License.txt'
    'Adobe-XMP-Toolkit-BSD-License.txt' = 'third_party\gpr\source\lib\xmp_core\BSD-License.txt'
    'Zstandard-LICENSE.txt'           = 'third_party\basis_universal\zstd\LICENSE'
}
foreach ($name in $licenses.Keys) { Copy-Item $licenses[$name] (Join-Path "$pkg\licenses" $name) }

# "Open with" registry entries for every image type Cyclops opens.
$exts = 'jpg jpeg jpe jfif png apng gif bmp webp tif tiff ico tga dds ktx ktx2 hdr exr qoi pnm svg psd ' +
        'heic heif hif avif jxl jxr dng cr2 cr3 crw nef nrw arw arq srf sr2 raf orf ori rw2 rwl pef srw x3f ' +
        'erf kdc dcr mrw 3fr fff iiq mef mos sti gpr'
$lines = foreach ($e in $exts -split ' ') {
    "Root: HKA; Subkey: ""Software\Classes\.$e\OpenWithProgids""; ValueType: string; ValueName: ""Cyclops.Image""; ValueData: """"; Flags: uninsdeletevalue; Tasks: openwith"
    "Root: HKA; Subkey: ""Software\Classes\Applications\cyclops.exe\SupportedTypes""; ValueType: string; ValueName: "".$e""; ValueData: """"; Tasks: openwith"
}
Set-Content -Encoding utf8 "$pkg\openwith.iss" $lines

$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found (winget install JRSoftware.InnoSetup)" }
& $iscc /Q "/DAppVersion=$version" installer\cyclops.iss
if ($LASTEXITCODE) { throw "ISCC failed" }
Get-Item "dist\Cyclops-$version-setup.exe"
