# Build the app and its installer: dist\Orchestrator-Setup-<version>.exe
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value

# Keep local paths (user name, folders) out of the binaries: remap source paths and
# store only the PDB's file name, not its full path.
$env:RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=~ --remap-path-prefix=$root=. -C link-arg=/PDBALTPATH:%_PDB%"

Push-Location $root
try {
    # via cmd: Windows PowerShell treats cargo's progress output on stderr as errors
    cmd /c "cargo build --release 2>&1"
    if ($LASTEXITCODE -ne 0) { throw "app build failed" }
    Push-Location (Join-Path $root 'installer')
    try {
        cmd /c "cargo build --release 2>&1"
        if ($LASTEXITCODE -ne 0) { throw "installer build failed" }
    } finally { Pop-Location }
} finally { Pop-Location }

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
$out = Join-Path $dist "Orchestrator-Setup-$version.exe"
Copy-Item (Join-Path $root 'installer\target\release\orchestrator-setup.exe') $out -Force
Write-Host "Built $out ($([math]::Round((Get-Item $out).Length / 1MB, 1)) MB)"
