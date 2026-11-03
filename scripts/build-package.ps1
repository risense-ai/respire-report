[CmdletBinding()]
param(
    [switch]$CheckOnly,
    [switch]$Offline,
    [string]$NodePath,
    [string]$PnpmPath,
    [string]$CargoHome,
    [string]$RustupHome,
    [string]$VcvarsPath
)
$ErrorActionPreference='Stop'
$repo=Split-Path -Parent $PSScriptRoot
$exitCode=1
$transcribing=$false
$originalEnvironment=@{}
foreach($key in @('PATH','CARGO_HOME','RUSTUP_HOME','AGENTNOTIFY_VCVARS','AGENTNOTIFY_PNPM','RUSTUP_TOOLCHAIN')){$originalEnvironment[$key]=[Environment]::GetEnvironmentVariable($key,'Process')}
function Run([string]$Program,[string[]]$Arguments){
    & $Program @Arguments
    if($LASTEXITCODE-ne 0){throw "Command failed (exit $LASTEXITCODE): $([IO.Path]::GetFileName($Program))"}
}
function Setting([string]$Explicit,[string]$EnvironmentName,[string]$LocalName){
    if($Explicit){return $Explicit}
    if($EnvironmentName){$value=[Environment]::GetEnvironmentVariable($EnvironmentName,'Process');if($value){return $value}}
    if($script:local -and $script:local.PSObject.Properties[$LocalName]){return [string]$script:local.$LocalName}
    return ''
}
function Resolve-Tool([string]$Explicit,[string]$Name,[string[]]$Candidates){
    if($Explicit){if(!(Test-Path -LiteralPath $Explicit -PathType Leaf)){throw "Explicit $Name path does not exist: $Explicit"};return (Resolve-Path -LiteralPath $Explicit).Path}
    $found=Get-Command $Name -ErrorAction SilentlyContinue | Select-Object -First 1
    if($found){return $found.Source}
    foreach($p in $Candidates){if($p -and (Test-Path -LiteralPath $p -PathType Leaf)){return $p}}
    return $null
}
Push-Location $repo
try {
    if($PSVersionTable.PSVersion.Major-lt 7){throw 'PowerShell 7 or newer is required. Use Build.cmd.'}
    if($env:OS-ne 'Windows_NT' -or ![Environment]::Is64BitProcess -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture-ne 'X64'){throw 'Windows x64 and x64 PowerShell are required.'}
    $logs=Join-Path $repo 'target/build-logs';New-Item -ItemType Directory -Path $logs -Force|Out-Null
    $log=Join-Path $logs ((Get-Date -Format 'yyyyMMdd-HHmmss')+'-'+[guid]::NewGuid().ToString('N')+'.log')
    Start-Transcript -LiteralPath $log -Force|Out-Null;$transcribing=$true
    Write-Host "Build log: $log"
    $script:local=$null;$localPath=Join-Path $repo '.build.local.json'
    if(Test-Path -LiteralPath $localPath){$script:local=Get-Content -LiteralPath $localPath -Raw|ConvertFrom-Json}
    $NodePath=Setting $NodePath 'AGENTNOTIFY_NODE' 'NodePath'
    $PnpmPath=Setting $PnpmPath 'AGENTNOTIFY_PNPM' 'PnpmPath'
    $CargoHome=Setting $CargoHome 'CARGO_HOME' 'CargoHome'
    $RustupHome=Setting $RustupHome 'RUSTUP_HOME' 'RustupHome'
    $VcvarsPath=Setting $VcvarsPath 'AGENTNOTIFY_VCVARS' 'VcvarsPath'
    if(!$CargoHome){$CargoHome=Join-Path $env:USERPROFILE '.cargo'}
    if(!$RustupHome){$RustupHome=Join-Path $env:USERPROFILE '.rustup'}
    $env:CARGO_HOME=$CargoHome;$env:RUSTUP_HOME=$RustupHome
    $env:PATH=(Join-Path $CargoHome 'bin')+';'+$env:PATH
    $node=Resolve-Tool $NodePath 'node.exe' @((Join-Path $env:ProgramFiles 'nodejs/node.exe'))
    if(!$node){throw 'Node.js is missing. Install Node 22.12+ (or 24+) and reopen Build.cmd; no global install was attempted.'}
    $env:PATH=(Split-Path -Parent $node)+';'+$env:PATH
    $nodeVersion=(& $node --version).Trim();if($LASTEXITCODE-ne 0){throw 'Node version probe failed.'}
    $nv=[version]$nodeVersion.TrimStart('v');if($nv-lt [version]'22.12.0'){throw "Node 22.12+ required; found $nodeVersion"}
    Write-Host "Node: $nodeVersion ($node)"
    $rustup=Resolve-Tool '' 'rustup.exe' @((Join-Path $CargoHome 'bin/rustup.exe'))
    $cargo=Resolve-Tool '' 'cargo.exe' @((Join-Path $CargoHome 'bin/cargo.exe'))
    if(!$rustup-or !$cargo){throw 'Rustup/Cargo missing. Install the Rust MSVC toolchain bootstrap, or set CargoHome and RustupHome in .build.local.json.'}
    $toolchain=(Select-String -LiteralPath 'rust-toolchain.toml' -Pattern '^channel\s*=\s*"([0-9.]+)"').Matches.Groups[1].Value
    if(!$toolchain){throw 'No exact Rust toolchain is pinned in rust-toolchain.toml.'}
    $installed=@(& $rustup toolchain list);if($LASTEXITCODE-ne 0){throw 'Rust toolchain inspection failed.'}
    $target="$toolchain-x86_64-pc-windows-msvc"
    if(!($installed|Where-Object {$_ -match ('^'+[regex]::Escape($target)+'(?:\s|$)')})){
        if($CheckOnly-or$Offline){throw "Pinned Rust $target missing. A normal online build can prepare it with rustup."}
        Run $rustup @('toolchain','install',$target,'--profile','minimal','--component','rustfmt','--component','clippy')
    }
    $components=@(& $rustup component list --toolchain $target --installed)
    if($LASTEXITCODE-ne 0){throw 'Rust component inspection failed.'}
    foreach($component in @('rustfmt','clippy')){
        if(!($components|Where-Object {$_ -match ('^'+[regex]::Escape($component)+'(?:-|\s|$)')})){
            if($CheckOnly-or$Offline){throw "Pinned Rust component missing: $component. Run an online build to prepare it."}
            Run $rustup @('component','add','--toolchain',$target,$component)
        }
    }
    $env:RUSTUP_TOOLCHAIN=$target
    Write-Host "Rust: $target"
    if(!$VcvarsPath){
        $vswhere=Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
        if(Test-Path -LiteralPath $vswhere){$vs=& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath;if($vs){$VcvarsPath=Join-Path $vs 'VC/Auxiliary/Build/vcvars64.bat'}}
    }
    if(!$VcvarsPath-or !(Test-Path -LiteralPath $VcvarsPath -PathType Leaf)){throw 'MSVC x64 Build Tools missing. Install Visual Studio C++ desktop build tools and a Windows SDK; this script does not install system toolchains.'}
    if($VcvarsPath -match '["\r\n]'){throw 'Unsupported characters in MSVC path.'}
    $probe=& $env:ComSpec /d /s /c "`"$VcvarsPath`" >nul && where cl.exe && where link.exe && where rc.exe"
    if($LASTEXITCODE-ne 0){throw 'MSVC or Windows SDK is incomplete: cl.exe, link.exe and rc.exe must be available in the x64 tool environment.'}
    $sdkOutput=& $env:ComSpec /d /s /c "`"$VcvarsPath`" >nul && set WindowsSdk && set VSCMD_ARG_TGT_ARCH"
    if($LASTEXITCODE-ne 0){throw 'Windows SDK environment is missing.'}
    $sdk=@{};foreach($line in $sdkOutput){if($line -match '^([^=]+)=(.*)$'){$sdk[$Matches[1]]=$Matches[2]}}
    if($sdk['VSCMD_ARG_TGT_ARCH']-ne 'x64' -or !$sdk['WindowsSdkDir'] -or !$sdk['WindowsSDKVersion']){throw 'MSVC must target x64 and provide WindowsSdkDir/WindowsSDKVersion.'}
    $sdkVersion=$sdk['WindowsSDKVersion'].TrimEnd([char]92,[char]47)
    foreach($relative in @("Include/$sdkVersion/um/Windows.h","Include/$sdkVersion/ucrt/stdio.h","Lib/$sdkVersion/um/x64/kernel32.lib","Lib/$sdkVersion/ucrt/x64/ucrt.lib")){
        if(!(Test-Path -LiteralPath (Join-Path $sdk['WindowsSdkDir'] $relative) -PathType Leaf)){throw "Windows SDK file missing: $relative"}
    }
    $env:AGENTNOTIFY_VCVARS=$VcvarsPath;Write-Host "MSVC x64 and Windows SDK $sdkVersion checked: $VcvarsPath"
    $package=Get-Content -LiteralPath 'apps/desktop/package.json' -Raw|ConvertFrom-Json
    if($package.packageManager -notmatch '^pnpm@([0-9]+\.[0-9]+\.[0-9]+)$'){throw 'Exact pnpm version missing from packageManager.'};$pnpmVersion=$Matches[1]
    $tools=Join-Path $repo 'target/build-tools'
    $pnpm=Resolve-Tool $PnpmPath 'pnpm.cmd' @((Join-Path $tools 'node_modules/.bin/pnpm.cmd'))
    $actual='';if($pnpm){$actual=(& $pnpm --version|Out-String).Trim();if($LASTEXITCODE-ne 0){$actual=''}}
    if($actual-ne $pnpmVersion){
        if($CheckOnly-or$Offline){throw "pnpm $pnpmVersion required; current version '$actual'. Normal online build can prepare a project-local copy."}
        $npm=Join-Path (Split-Path -Parent $node) 'npm.cmd';if(!(Test-Path -LiteralPath $npm)){throw 'npm.cmd missing beside Node; cannot prepare pinned pnpm.'}
        Run $npm @('install','--prefix',$tools,'--no-save','--package-lock=false',"pnpm@$pnpmVersion")
        $pnpm=Join-Path $tools 'node_modules/.bin/pnpm.cmd';$actual=(& $pnpm --version|Out-String).Trim();if($LASTEXITCODE-ne 0-or$actual-ne $pnpmVersion){throw 'Pinned pnpm preparation failed.'}
    }
    $env:AGENTNOTIFY_PNPM=$pnpm;Write-Host "pnpm: $actual ($pnpm)"
    if($CheckOnly){Write-Host 'CHECK PASSED: tool prerequisites found. Dependencies were not fetched, and no executable/package was built.';$exitCode=0}
    else {
        $installArgs=@('-C','apps/desktop','install','--frozen-lockfile');if($Offline){$installArgs+='--offline'}
        Run $pnpm $installArgs
        $fetch=@('fetch','--locked');if($Offline){$fetch+='--offline'}
        Run $cargo $fetch
        Run (Join-Path $PSHOME 'pwsh.exe') @('-NoLogo','-NoProfile','-File',(Join-Path $PSScriptRoot 'package.ps1'),'-Channel','Development')
        Write-Host 'BUILD SUCCEEDED. See EXECUTABLE and DOCUMENTATION paths above.';$exitCode=0
    }
} catch {Write-Host ('BUILD FAILED: '+$_.Exception.Message) -ForegroundColor Red;Write-Host 'Nothing was installed into AgentNotify. See the build log and docs/release/build-from-source.md.'}
finally {if($transcribing){Stop-Transcript|Out-Null};foreach($key in $originalEnvironment.Keys){[Environment]::SetEnvironmentVariable($key,$originalEnvironment[$key],'Process')};Pop-Location}
exit $exitCode
