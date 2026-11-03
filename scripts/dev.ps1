$CargoArgs = if ($args.Count) { $args } else { @('build', '--workspace', '--locked') }
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location $projectRoot
try {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'cargo is absent from PATH; set PATH, CARGO_HOME and RUSTUP_HOME for your installed Rust toolchain.' }
    if ($IsWindows -and ($env:AGENTNOTIFY_VCVARS -or -not (Get-Command cl.exe -ErrorAction SilentlyContinue))) {
        $vcvars = $env:AGENTNOTIFY_VCVARS
        if (-not $vcvars) {
            $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
            if (Test-Path -LiteralPath $vswhere) {
                $install = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
                if ($install) { $vcvars = Join-Path $install 'VC/Auxiliary/Build/vcvars64.bat' }
            }
        }
        if (-not $vcvars -or -not (Test-Path -LiteralPath $vcvars)) { throw 'MSVC environment absent. Run in an x64 Native Tools shell, or set AGENTNOTIFY_VCVARS to vcvars64.bat.' }
        # Import only this process environment; do not change global PATH/toolchains.
        $environment = & $env:ComSpec /d /s /c "`"$vcvars`" >nul && set"
        if ($LASTEXITCODE -ne 0) { throw 'vcvars64.bat failed.' }
        foreach ($line in $environment) {
            if ($line -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process') }
        }
    }
    & cargo @CargoArgs
    exit $LASTEXITCODE
} finally { Pop-Location }
