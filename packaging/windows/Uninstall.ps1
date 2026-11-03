$ErrorActionPreference = 'Stop'
try {
    $roots = @((Join-Path ([Environment]::GetFolderPath('UserProfile')) 'AgentNotify'), (Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'AgentNotify'))
    $installed = @($roots | Select-Object -Unique | Where-Object { Test-Path -LiteralPath (Join-Path $_ 'installation.json') -PathType Leaf })
    if ($installed.Count -ne 1) { throw 'Exactly one installed AgentNotify data directory is required. Ask your Agent to select the intended installation explicitly.' }
    $dataDir = $installed[0]
    $cli = Join-Path $dataDir 'launcher.exe'
    Write-Host 'Remove only the owned AgentNotify login task. Data, credentials and client hooks will be preserved.'
    & $cli --data-dir $dataDir uninstall --plan
    if ($LASTEXITCODE -ne 0) { throw 'Cannot verify installation ownership. No uninstall was performed.' }
    $answer = Read-Host 'Type UNINSTALL to stop/remove the AgentNotify background task'
    if ($answer -cne 'UNINSTALL') { Write-Host 'Cancelled.'; exit 0 }
    & $cli --data-dir $dataDir uninstall --apply
    if ($LASTEXITCODE -ne 0) { throw 'Uninstall did not finish. Preserve the data folder and ask your Agent to inspect ownership.' }
    Write-Host 'Background task removed. Data, credentials and hooks remain. Ask your Agent to remove ONLY matching AgentNotify hooks before manually removing binaries.'
    exit 0
} catch { Write-Host $_.Exception.Message -ForegroundColor Red; exit 1 }
