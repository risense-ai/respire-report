<# Windows bootstrap. Validates the package, finds a usable Python, then runs the
   idempotent installer. -InstallPython is explicit permission for winget only;
   -DryRun never installs Python. No policy/trust/global environment changes. #>
[CmdletBinding()]
param(
    [string]$TargetDir,
    [string]$PythonPath,
    [string]$CodexHome,
    [switch]$DryRun,
    [switch]$InstallPython,
    [switch]$CreateShortcut
)
$ErrorActionPreference = 'Stop'

function Test-PackageIntegrity {
    # 固定允许复制的文件；manifest 中额外路径不能获得复制权限。
    $allowed = @('notifier.py', '保存授权码.ps1', 'settings.example.json', 'Install.ps1',
        'install_notifier.py', 'AGENTS.md', 'START_HERE.md', 'README.md',
        'Codex邮件通知器使用与迁移说明.md', 'tests/test_notifier.py', 'tests/test_install_notifier.py')
    $manifestPath = Join-Path $PSScriptRoot 'manifest.sha256.json'
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw 'Package manifest is missing.' }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($manifest.schema_version -ne 1 -or $null -eq $manifest.files) { throw 'Unsupported manifest schema.' }
    $names = @($manifest.files.PSObject.Properties.Name)
    if ($names.Count -ne $allowed.Count -or @(Compare-Object $allowed $names -CaseSensitive).Count -ne 0) {
        throw 'Manifest must contain exactly the fixed package file allowlist.'
    }
    foreach ($name in $allowed) {
        $path = Join-Path $PSScriptRoot $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing package file: $name" }
        $expected = [string]$manifest.files.$name
        $sha256 = [System.Security.Cryptography.SHA256]::Create()
        try {
            $actual = [BitConverter]::ToString($sha256.ComputeHash([IO.File]::ReadAllBytes($path))).Replace('-', '')
        } finally { $sha256.Dispose() }
        if ($expected -notmatch '^[a-fA-F0-9]{64}$' -or $actual -ne $expected) { throw "Package SHA256 mismatch: $name" }
    }
}

function Get-UsablePython([string]$Candidate, [string]$LauncherOption = '') {
    # 实际执行版本和标准库探针；避免 Windows Store alias 或挂起的解释器阻塞安装。
    if (-not $Candidate -or -not (Test-Path -LiteralPath $Candidate -PathType Leaf)) { return $null }
    if ($Candidate -match '[\\/]WindowsApps[\\/]') { return $null }
    $probe = "import sys,tkinter,sqlite3,ssl,ctypes,json; assert sys.version_info >= (3,12); assert sys.platform == 'win32'; print(json.dumps({'executable':sys.executable}))"
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = $Candidate
    $start.Arguments = "$LauncherOption -c `"$probe`""
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $start
    try {
        if (-not $process.Start()) { return $null }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(20000)) { $process.Kill(); return $null }
        if ($process.ExitCode -ne 0) { return $null }
        $result = $stdout.Result | ConvertFrom-Json
        if ($result.executable -and (Test-Path -LiteralPath $result.executable -PathType Leaf)) {
            return [string]$result.executable
        }
    } catch {
        return $null
    } finally {
        $process.Dispose()
    }
    return $null
}

function Find-Python {
    # 1. 显式解释器失败即停止，不暗中换用其他解释器或安装依赖。
    if ($PythonPath) {
        $found = Get-UsablePython $PythonPath
        if (-not $found) { throw 'Explicit -PythonPath must be a usable Windows Python 3.12+ with tkinter/sqlite3/ssl/ctypes.' }
        return $found
    }
    # 2. 使用 Python 启动器、PATH 和当前用户安装目录进行可迁移探测。
    $launcher = Get-Command py.exe -ErrorAction SilentlyContinue
    if ($launcher) {
        foreach ($version in @('-3.13', '-3')) {
            $found = Get-UsablePython $launcher.Source $version
            if ($found) { return $found }
        }
    }
    foreach ($name in @('python.exe', 'python3.exe')) {
        foreach ($candidate in @(Get-Command $name -All -ErrorAction SilentlyContinue)) {
            $found = Get-UsablePython $candidate.Source
            if ($found) { return $found }
        }
    }
    if ($env:LOCALAPPDATA) {
        $pythonRoot = Join-Path $env:LOCALAPPDATA 'Programs\Python'
        if (Test-Path -LiteralPath $pythonRoot -PathType Container) {
            foreach ($folder in @(Get-ChildItem -LiteralPath $pythonRoot -Directory | Sort-Object Name -Descending)) {
                $found = Get-UsablePython (Join-Path $folder.FullName 'python.exe')
                if ($found) { return $found }
            }
        }
    }
    return $null
}

function Set-SettingsShortcut($Report) {
    # 快捷方式仅限安装目录；已有一致内容保持不写，变更前保留原文件。
    $linkPath = Join-Path $Report.target_dir '通知设置.lnk'
    $linkPython = Join-Path (Split-Path -Parent $Report.python_path) 'pythonw.exe'
    if (-not (Test-Path -LiteralPath $linkPython -PathType Leaf)) { $linkPython = $Report.python_path }
    $arguments = '"' + (Join-Path $Report.target_dir 'notifier.py') + '" settings'
    if ($DryRun) {
        $Report | Add-Member -NotePropertyName shortcut_path -NotePropertyValue $linkPath
        $Report | Add-Member -NotePropertyName shortcut_planned -NotePropertyValue $true
        return
    }
    $shell = New-Object -ComObject WScript.Shell
    try {
        $link = $shell.CreateShortcut($linkPath)
        if ((Test-Path -LiteralPath $linkPath -PathType Leaf) -and $link.TargetPath -eq $linkPython -and
            $link.Arguments -eq $arguments -and $link.WorkingDirectory -eq $Report.target_dir) {
            $Report.unchanged += $linkPath
            return
        }
        if (Test-Path -LiteralPath $linkPath) {
            $backup = $linkPath + '.bak-' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffffffZ')
            Copy-Item -LiteralPath $linkPath -Destination $backup
            $Report.backups += $backup
        }
        $link.TargetPath = $linkPython
        $link.Arguments = $arguments
        $link.WorkingDirectory = $Report.target_dir
        $link.Description = 'Codex 邮件通知设置'
        # 与工具文件一致，快捷方式先写同目录临时文件，再原子替换。
        $temporaryLinkPath = Join-Path $Report.target_dir ('.notifier-install-' + [Guid]::NewGuid().ToString('N') + '.lnk')
        $temporaryLink = $null
        try {
            $temporaryLink = $shell.CreateShortcut($temporaryLinkPath)
            $temporaryLink.TargetPath = $link.TargetPath
            $temporaryLink.Arguments = $link.Arguments
            $temporaryLink.WorkingDirectory = $link.WorkingDirectory
            $temporaryLink.Description = $link.Description
            $temporaryLink.Save()
            if (Test-Path -LiteralPath $linkPath -PathType Leaf) {
                [IO.File]::Replace($temporaryLinkPath, $linkPath, $null)
            } else {
                [IO.File]::Move($temporaryLinkPath, $linkPath)
            }
        } finally {
            if ($null -ne $temporaryLink) { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($temporaryLink) }
            if (Test-Path -LiteralPath $temporaryLinkPath -PathType Leaf) { Remove-Item -LiteralPath $temporaryLinkPath }
        }
        $Report.changed += $linkPath
    } finally {
        if ($null -ne $link) { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($link) }
        [void][Runtime.InteropServices.Marshal]::ReleaseComObject($shell)
    }
}

# 1. 包完整性必须先于可选依赖安装，损坏包不应引发宿主修改。
if ($env:OS -ne 'Windows_NT') { throw 'This package requires Windows.' }
Test-PackageIntegrity
$selectedPython = Find-Python
if (-not $selectedPython) {
    if (-not $InstallPython) {
        throw 'No usable Python found. Supply -PythonPath, or explicitly allow -InstallPython (winget Python.Python.3.13, user scope).'
    }
    if ($DryRun) { throw 'DryRun: Python installation would be required; no installation was performed. Supply a usable -PythonPath to preview the notifier plan.' }
    $winget = Get-Command winget.exe -ErrorAction SilentlyContinue
    if (-not $winget) { throw 'winget is unavailable. Have the agent obtain Python 3.13 from the official Python distribution, then rerun with -PythonPath.' }
    & $winget.Source install --id Python.Python.3.13 --exact --source winget --scope user --silent --accept-package-agreements --accept-source-agreements --disable-interactivity
    if ($LASTEXITCODE -ne 0) { throw "winget failed with exit code $LASTEXITCODE; no notifier files or hooks were installed." }
    $selectedPython = Find-Python
    if (-not $selectedPython) { throw 'Python was installed but a usable interpreter was not found. Rerun with its explicit -PythonPath.' }
}

# 2. Python 执行完整预检与写入，输出唯一 JSON 报告供 agent 读取。
$installerArgs = @((Join-Path $PSScriptRoot 'install_notifier.py'), '--python-path', $selectedPython)
if ($TargetDir) { $installerArgs += @('--target-dir', $TargetDir) }
if ($CodexHome) { $installerArgs += @('--codex-home', $CodexHome) }
if ($DryRun) { $installerArgs += '--dry-run' }
$previousOutputEncoding = [Console]::OutputEncoding
try {
    [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    $output = & $selectedPython @installerArgs
    if ($LASTEXITCODE -ne 0) { throw "Notifier installer failed with exit code $LASTEXITCODE. Review its error above; no Hook trust was granted." }
    $report = ($output -join "`n") | ConvertFrom-Json
    if ($CreateShortcut) { Set-SettingsShortcut $report }
    $report | ConvertTo-Json -Depth 20
} finally {
    [Console]::OutputEncoding = $previousOutputEncoding
}
