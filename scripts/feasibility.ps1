[CmdletBinding()]
param([ValidateSet('Baseline', 'Verify')][string]$Mode = 'Verify',
      [ValidateSet('Windows', 'macOS', 'Linux')][string]$Platform = 'Windows')
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$artifactRoot = Join-Path $projectRoot 'test-artifacts/m0'
New-Item -ItemType Directory -Path $artifactRoot -Force | Out-Null
$osBuild = [Environment]::OSVersion.VersionString
if ($IsWindows) {
    $osInfo = Get-ItemProperty -LiteralPath 'HKLM:/SOFTWARE/Microsoft/Windows NT/CurrentVersion'
    $osBuild = "Windows NT 10.0.$($osInfo.CurrentBuild).$($osInfo.UBR) (DisplayVersion=$($osInfo.DisplayVersion))"
}
$tools = @{}
foreach ($name in @('cargo', 'rustc', 'node', 'pnpm')) {
    $command = Get-Command $name -ErrorAction SilentlyContinue
    $tools[$name] = if ($command) { (& $command.Source --version 2>&1 | Out-String).Trim() } else { 'absent from PATH' }
}
$webview = if ($IsWindows) {
    @(Get-ItemProperty -Path 'HKLM:/SOFTWARE/WOW6432Node/Microsoft/EdgeUpdate/Clients/*', 'HKCU:/SOFTWARE/Microsoft/EdgeUpdate/Clients/*' -ErrorAction SilentlyContinue |
      Where-Object name -Like '*WebView2*' | Select-Object -ExpandProperty pv)
} else { @() }
$nativeTools = [ordered]@{
    cl_in_current_path = [bool](Get-Command cl.exe -ErrorAction SilentlyContinue)
    vcvars_override_available = [bool]($env:AGENTNOTIFY_VCVARS -and (Test-Path -LiteralPath $env:AGENTNOTIFY_VCVARS))
    sdk_registry_available = if ($IsWindows) { [bool](Test-Path -LiteralPath 'HKLM:/SOFTWARE/Microsoft/Windows Kits/Installed Roots') } else { $false }
}
$report = [ordered]@{ mode=$Mode; platform=$Platform; os_build=$osBuild; tools=$tools; native_tools=$nativeTools; webview2=$webview; probes=@() }
$probeNames = @('credential_real_roundtrip','smtp_local_tls_probe','smtp_data_stage_observable','service_gui_independence')
function New-Observation([string]$Name, [string]$Status, [string]$Actual, [string]$Evidence) {
    return [ordered]@{probe=$Name;os_build=$osBuild;backend='unverified';dependency_version='see Cargo.lock';features=@();actual=$Actual;evidence=$Evidence;status=$Status}
}
if ($Mode -eq 'Baseline') {
    foreach ($name in $probeNames) {
        $report.probes += New-Observation $name 'not_run' 'Baseline: no capability accepted before experiment' 'baseline.json'
    }
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $artifactRoot 'baseline.json') -Encoding utf8
    Write-Output 'Baseline records missing/unaccepted probes; exit 2 is intentional, not a passing functional test.'
    exit 2
}
if (-not $IsWindows -or $Platform -ne 'Windows') {
    foreach ($name in $probeNames) {
        $report.probes += New-Observation $name 'blocked' 'M0 native platform gate requires a real Windows host; macOS/Linux have not been exercised.' 'verify.json'
    }
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $artifactRoot 'verify.json') -Encoding utf8
    exit 3
}
$env:AGENTNOTIFY_OS_BUILD = $osBuild
$env:AGENTNOTIFY_EVIDENCE_DIR = $artifactRoot
$failures = 0
$targets = [ordered]@{
    credential_real = @('credential_real_roundtrip')
    service_real = @('service_gui_independence')
    smtp_local = @('smtp_local_tls_probe', 'smtp_data_stage_observable')
}
foreach ($target in $targets.Keys) {
    # Replace prior observations before each run so stale successes cannot survive a failure.
    foreach ($name in $targets[$target]) {
        New-Observation $name 'not_run' 'Probe execution has not returned' "$target.txt" |
            ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $artifactRoot "$name.json") -Encoding utf8
    }
    $output = & (Join-Path $PSScriptRoot 'dev.ps1') test --locked --test $target '--' --nocapture 2>&1
    $code = $LASTEXITCODE
    $output | Set-Content -LiteralPath (Join-Path $artifactRoot "$target.txt") -Encoding utf8
    $output | Write-Output
    foreach ($name in $targets[$target]) {
        $observationPath = Join-Path $artifactRoot "$name.json"
        $observation = Get-Content -LiteralPath $observationPath -Raw | ConvertFrom-Json -AsHashtable
        if ($code -ne 0 -or $observation.status -ne 'passed') {
            $observation = New-Observation $name 'failed' "Probe target failed or produced no valid native result; exit=$code" "$target.txt"
            $observation | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $observationPath -Encoding utf8
            $failures++
        }
        $observation['exit_code'] = $code
        $report.probes += $observation
    }
}
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $artifactRoot 'verify.json') -Encoding utf8
if ($failures) { exit 1 }
exit 0
