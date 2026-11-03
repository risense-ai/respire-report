param([switch]$Install, [string]$DataDir)
$ErrorActionPreference = 'Stop'
try {
    if (-not [Environment]::Is64BitOperatingSystem) { throw 'AgentNotify requires Windows x64.' }
    # Installed data roots are explicit. Never silently select between two installations.
    if (-not $DataDir) {
        $profileRoot = Join-Path ([Environment]::GetFolderPath('UserProfile')) 'AgentNotify'
        $localRoot = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'AgentNotify'
        $installed = @(@($profileRoot, $localRoot) | Select-Object -Unique | Where-Object { Test-Path -LiteralPath (Join-Path $_ 'installation.json') -PathType Leaf })
        if ($installed.Count -gt 1) { throw 'Multiple AgentNotify installations found. Use the desktop shortcut for the intended data directory, or pass -DataDir explicitly.' }
        $DataDir = if ($installed.Count -eq 1) { $installed[0] } else { $profileRoot }
    }
    if (-not [IO.Path]::IsPathRooted($DataDir) -or $DataDir.Contains('"')) { throw 'An absolute data directory is required.' }
    $DataDir = [IO.Path]::GetFullPath($DataDir)
    # All three executables stay together in bin for native installation discovery.
    $packageRoot = Split-Path -Parent $PSScriptRoot
    $cli = Join-Path $packageRoot 'bin/agentnotify.exe'
    $launcher = Join-Path $DataDir 'launcher.exe'
    $hasInstallation = Test-Path -LiteralPath (Join-Path $DataDir 'installation.json') -PathType Leaf
    if ($hasInstallation) { $cli = $launcher }
    if (-not (Test-Path -LiteralPath $cli -PathType Leaf)) { throw 'Package is incomplete. Extract the entire ZIP first.' }
    if ($Install) {
        # Install is explicit; saving credentials or enabling notifications is not part of installation.
        $report = & $cli --data-dir $DataDir service install | Out-String
        if ($LASTEXITCODE -ne 0) { throw 'Background install failed. Existing foreign tasks were not replaced.' }
        $cli = $launcher
        $hasInstallation = $true
    }
    if ($hasInstallation) {
        $report = & $cli --data-dir $DataDir service status | Out-String
        if ($LASTEXITCODE -ne 0) { throw 'Installation ownership could not be verified. Ask your Agent to inspect this data directory.' }
        # A single daily entry points to the stable validated launcher, not a versioned executable.
        $shell = New-Object -ComObject WScript.Shell
        $linkPath = Join-Path ([Environment]::GetFolderPath('Desktop')) 'AgentNotify.lnk'
        $link = $shell.CreateShortcut($linkPath)
        if ((Test-Path -LiteralPath $linkPath) -and $link.TargetPath -and -not [string]::Equals($link.TargetPath, $launcher, [StringComparison]::OrdinalIgnoreCase)) { throw 'An AgentNotify desktop shortcut already points elsewhere; it was not overwritten.' }
        $link.TargetPath = $launcher
        $link.Arguments = '--data-dir "' + $DataDir + '" desktop'
        $link.WorkingDirectory = $DataDir
        $record = Get-Content -LiteralPath (Join-Path $DataDir 'installation.json') -Raw | ConvertFrom-Json
        $link.IconLocation = (Join-Path (Split-Path -Parent $record.daemon) 'agentnotify-desktop.exe') + ',0'
        $link.Description = 'AgentNotify settings and notification tray'
        $link.Save()
    }
    $report = & $cli --data-dir $DataDir desktop | Out-String
    if ($LASTEXITCODE -ne 0) { throw 'Desktop could not be opened. Preserve the data folder and inspect installation ownership.' }
    exit 0
} catch {
    Add-Type -AssemblyName System.Windows.Forms
    [void][System.Windows.Forms.MessageBox]::Show($_.Exception.Message, 'AgentNotify', 'OK', 'Error')
    exit 1
}
