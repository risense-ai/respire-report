$ErrorActionPreference = 'Stop'
$settings = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'settings.json') -Raw -Encoding UTF8 | ConvertFrom-Json
if (-not $settings.sender) { throw '请先在通知设置中填写发送邮箱。' }
$secret = Read-Host '请输入 SMTP 授权码（输入不显示）' -AsSecureString
$credential = [System.Management.Automation.PSCredential]::new($settings.sender, $secret)
$credential | Export-Clixml -LiteralPath (Join-Path $PSScriptRoot 'smtp.credential.xml')
Write-Host '授权码已由 Windows DPAPI 加密保存，仅当前 Windows 用户可解密。'
