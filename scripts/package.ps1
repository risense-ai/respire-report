param(
    [ValidateSet('Windows')][string]$Platform = 'Windows',
    [ValidateSet('Development','Test','Release')][string]$Channel = 'Development',
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if ($env:OS -ne 'Windows_NT' -or -not [Environment]::Is64BitOperatingSystem) { throw 'Windows x64 build host required.' }
# No private key paths/values are accepted. Release signing belongs to the publisher pipeline.
if ($Channel -eq 'Release') { throw 'Formal release packaging is disabled until publisher trust root, OS signing and independent release approval are configured. Use Development.' }
Push-Location $root
try {
    if (-not $SkipBuild) {
        $pnpm = if ($env:AGENTNOTIFY_PNPM) { $env:AGENTNOTIFY_PNPM } else { 'pnpm.cmd' }
        & $pnpm -C apps/desktop build
        if ($LASTEXITCODE -ne 0) { throw 'Frontend production build failed.' }
        $oldFlags = $env:RUSTFLAGS
        try {
            # Static CRT avoids requiring users to install developer tools/VC runtimes.
            $env:RUSTFLAGS = (($oldFlags + ' -C target-feature=+crt-static').Trim())
            & (Join-Path $PSHOME 'pwsh.exe') -NoLogo -NoProfile -File (Join-Path $PSScriptRoot 'dev.ps1') build --release --offline --locked -p agentnotify-desktop --features agentnotify-desktop/custom-protocol
            if ($LASTEXITCODE -ne 0) { throw 'Native production build failed.' }
        } finally { $env:RUSTFLAGS = $oldFlags }
    }
    $version = (Select-String -LiteralPath 'Cargo.toml' -Pattern '^version = "([0-9.]+)"$' | Select-Object -First 1).Matches.Groups[1].Value
    if (-not $version) { throw 'Workspace version missing.' }
    $name = 'AgentNotify-' + $version + '-windows-x64-unsigned-dev-' + (Get-Date -Format 'yyyyMMdd-HHmmss')
    $out = Join-Path $root ('target/packages/' + $name)
    if (Test-Path -LiteralPath $out) { throw 'Package destination exists; refusing overwrite.' }
    # 1. One linked executable; documentation is never a runtime dependency.
    $source = Join-Path $root 'target/release/agentnotify-desktop.exe'
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw 'Missing unified production binary.' }
    New-Item -ItemType Directory -Path $out | Out-Null
    $executable = Join-Path $out 'AgentNotify.exe'
    Copy-Item -LiteralPath $source -Destination $executable
    $runtimeHash = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
    # 2. Keep documentation staging outside the delivered two-file directory.
    $docs = Join-Path $root ('target/package-staging/' + $name + '-docs')
    if (Test-Path -LiteralPath $docs) { throw 'Documentation staging exists; refusing overwrite.' }
    New-Item -ItemType Directory -Path $docs | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $docs 'meta') | Out-Null
    foreach ($file in @('START_HERE.md','AGENT_HANDOFF.md')) { Copy-Item -LiteralPath (Join-Path $root ('packaging/windows/' + $file)) -Destination (Join-Path $docs $file) }
    Copy-Item -LiteralPath 'LICENSE' -Destination (Join-Path $docs 'LICENSE')
    Copy-Item -LiteralPath 'docs/release/offline-install.md' -Destination (Join-Path $docs 'README.md')
    Copy-Item -LiteralPath 'docs/release/signing.md' -Destination (Join-Path $docs 'SIGNING.md')
    Copy-Item -LiteralPath 'docs/release/release-checklist.md' -Destination (Join-Path $docs 'MANUAL_CHECKLIST.md')
    Copy-Item -LiteralPath 'docs/migration/windows.md' -Destination (Join-Path $docs 'MIGRATION.md')
    $metadata = @{ version=$version; platform='windows-x64'; channel='unsigned-development'; layout='single-executable'; executable='AgentNotify.exe'; executable_sha256=$runtimeHash; operating_system_signature='not_verified'; update_signature='absent'; automatically_updatable=$false; built_utc=[DateTime]::UtcNow.ToString('o'); static_crt_requested=$true; runtime_acceptance='pending_user_validation' } | ConvertTo-Json
    [IO.File]::WriteAllText((Join-Path $docs 'meta/BUILD-INFO.json'), $metadata, [Text.UTF8Encoding]::new($false))
    # This checksum refers to the separately delivered EXE, not a ZIP member.
    [IO.File]::WriteAllText((Join-Path $docs 'meta/RUNTIME-SHA256.txt'), ($runtimeHash + '  AgentNotify.exe' + [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
    # 3. Internal checksums are relative to the documentation extraction root.
    $docsRoot = [IO.Path]::GetFullPath($docs).TrimEnd([char]92, [char]47) + [IO.Path]::DirectorySeparatorChar
    $rows = @(Get-ChildItem -LiteralPath $docs -Recurse -File | Sort-Object FullName | ForEach-Object {
        $absolute = [IO.Path]::GetFullPath($_.FullName)
        if (($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -or -not $absolute.StartsWith($docsRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe documentation checksum path.' }
        $relative = $absolute.Substring($docsRoot.Length).Replace('\', '/')
        if ($relative -match '(^|/)\.\.(/|$)|[\r\n]' -or [IO.Path]::IsPathRooted($relative)) { throw 'Unsafe relative checksum path.' }
        (Get-FileHash -LiteralPath $absolute -Algorithm SHA256).Hash.ToLowerInvariant() + '  ' + $relative
    })
    [IO.File]::WriteAllLines((Join-Path $docs 'meta/SHA256SUMS.txt'), $rows, [Text.UTF8Encoding]::new($false))
    $archive = Join-Path $out 'AgentNotify-docs.zip'
    $docItems = @(Get-ChildItem -LiteralPath $docs | ForEach-Object FullName)
    Compress-Archive -LiteralPath $docItems -DestinationPath $archive -CompressionLevel Optimal
    $archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-Output ('EXECUTABLE=' + $executable)
    Write-Output ('EXECUTABLE_SHA256=' + $runtimeHash)
    Write-Output ('DOCUMENTATION=' + $archive)
    Write-Output ('DOCUMENTATION_SHA256=' + $archiveHash)
} finally { Pop-Location }
