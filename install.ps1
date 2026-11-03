# Build Orchestrator in release mode and install it for the current user:
#   %LOCALAPPDATA%\Programs\Orchestrator\orchestrator.exe
#   Start menu shortcut + desktop shortcut
# Re-run after pulling changes to update the installed app.

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Orchestrator'

Push-Location $root
try {
    # via cmd: Windows PowerShell treats cargo's progress output on stderr as errors
    cmd /c "cargo build --release 2>&1"
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally { Pop-Location }

$running = Get-Process orchestrator -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $dest 'orchestrator.exe') }
if ($running) { throw "Orchestrator is running. Close it, then run this again." }

New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item (Join-Path $root 'target\release\orchestrator.exe') $dest -Force

$shell = New-Object -ComObject WScript.Shell
foreach ($dir in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) {
    $path = Join-Path $dir 'Orchestrator.lnk'
    # Recreate rather than overwrite: Start doesn't notice a shortcut rewritten in place.
    if (Test-Path $path) { Remove-Item $path -Force; Start-Sleep -Milliseconds 800 }
    $lnk = $shell.CreateShortcut($path)
    $lnk.TargetPath = Join-Path $dest 'orchestrator.exe'
    $lnk.WorkingDirectory = $dest
    $lnk.Description = 'Orchestrator: parallel Claude Code threads'
    $lnk.IconLocation = (Join-Path $dest 'orchestrator.exe') + ',0'
    $lnk.Save()
}
Write-Host "Installed to $dest (Start menu + desktop shortcut)"
