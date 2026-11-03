$ErrorActionPreference='Stop'
[Console]::InputEncoding=[System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)
function Resolve-AccountSid([string]$identity) {
    if ([string]::IsNullOrWhiteSpace($identity)) { throw 'identity_missing' }
    if ($identity -match '^S-1-') {
        return [System.Security.Principal.SecurityIdentifier]::new($identity).Value
    }
    return [System.Security.Principal.NTAccount]::new($identity).Translate([System.Security.Principal.SecurityIdentifier]).Value
}
function Same-AbsolutePath([string]$left,[string]$right) {
    # Only rooted paths are accepted; separator and drive-letter case are representation details.
    if ($left -match '\A\\\\[?.][\\/]' -or $right -match '\A\\\\[?.][\\/]') { return $false }
    if ($left -match '(?:\A|[\\/])\.\.(?:[\\/]|\z)' -or $right -match '(?:\A|[\\/])\.\.(?:[\\/]|\z)') { return $false }
    $absolute='\A(?:[A-Za-z]:[\\/]|\\\\[^\\/]+[\\/][^\\/]+(?:[\\/]|\z))'
    if ([string]::IsNullOrWhiteSpace($left) -or [string]::IsNullOrWhiteSpace($right) -or $left -notmatch $absolute -or $right -notmatch $absolute) { return $false }
    return [string]::Equals([IO.Path]::GetFullPath($left).TrimEnd('\','/'),[IO.Path]::GetFullPath($right).TrimEnd('\','/'),[StringComparison]::OrdinalIgnoreCase)
}
function Same-DaemonArguments([string]$actual,[string]$expected) {
    # Preserve the exact command grammar. Only its quoted absolute data path can normalize.
    $pattern='\A--data-dir "([^"\r\n]+)" launch-daemon\z'
    $a=[regex]::Match($actual,$pattern);$e=[regex]::Match($expected,$pattern)
    if (!$a.Success -or !$e.Success) { return $false }
    # A trailing backslash escapes the closing quote under Windows argv parsing.
    if ($a.Groups[1].Value.EndsWith('\') -or $e.Groups[1].Value.EndsWith('\')) { return $false }
    return (Same-AbsolutePath $a.Groups[1].Value $e.Groups[1].Value)
}
try {
    $r=[Console]::In.ReadToEnd()|ConvertFrom-Json
    $name='AgentNotify.'+$r.sid
    $svc=New-Object -ComObject 'Schedule.Service'
    $svc.Connect()
    $folder=$svc.GetFolder('\')
    $task=$null
    try {$task=$folder.GetTask($name)} catch {if ($_.Exception.HResult -ne -2147024894){throw}}
    # Missing manifest is first-use only when no current-user task exists; never adopt it.
    if ($r.action -eq 'probe-uninstalled') {
        if ($task) { throw 'unowned_task' }
        @{installed=$false;running=$false;unsupported=$false;failed=$false;code='not_installed'}|ConvertTo-Json -Compress
        exit 0
    }
    if ($task) {
        $d=$task.Definition
        $sd=[System.Security.AccessControl.RawSecurityDescriptor]::new($task.GetSecurityDescriptor(7))
        if ($sd.Owner.Value -ne $r.sid) {throw 'owner_conflict'}
        foreach($ace in $sd.DiscretionaryAcl){
            if ($ace -isnot [System.Security.AccessControl.CommonAce]){throw 'unsupported_acl'}
            if ($ace.AceQualifier -eq [System.Security.AccessControl.AceQualifier]::AccessAllowed -and $ace.SecurityIdentifier.Value -notin @($r.sid,'S-1-5-18','S-1-5-32-544')){throw 'acl_conflict'}
        }
        if ($null -eq $sd.DiscretionaryAcl -or $d.Data -ne ('AgentNotify/v1/'+$r.id) -or (Resolve-AccountSid $d.Principal.UserId) -ne $r.sid -or $d.Principal.LogonType -ne 3 -or $d.Principal.RunLevel -ne 0 -or $d.Actions.Count -ne 1 -or $d.Actions.Item(1).Type -ne 0 -or !(Same-AbsolutePath $d.Actions.Item(1).Path $r.launcher) -or !(Same-DaemonArguments $d.Actions.Item(1).Arguments $r.arguments) -or !(Same-AbsolutePath $d.Actions.Item(1).WorkingDirectory $r.root) -or $d.Triggers.Count -ne 1 -or $d.Triggers.Item(1).Type -ne 9 -or (Resolve-AccountSid $d.Triggers.Item(1).UserId) -ne $r.sid){throw 'definition_conflict'}
    }
    if ($r.action -eq 'install' -and !$task){
        $d=$svc.NewTask(0);$d.Data='AgentNotify/v1/'+$r.id
        $d.RegistrationInfo.Author='AgentNotify'
        $d.Principal.UserId=$r.sid;$d.Principal.LogonType=3;$d.Principal.RunLevel=0
        $trigger=$d.Triggers.Create(9);$trigger.UserId=$r.sid;$trigger.Enabled=$true
        $a=$d.Actions.Create(0);$a.Path=$r.launcher;$a.Arguments=$r.arguments;$a.WorkingDirectory=$r.root
        $d.Settings.Enabled=$true;$d.Settings.AllowDemandStart=$true;$d.Settings.StartWhenAvailable=$true
        $d.Settings.ExecutionTimeLimit='PT0S';$d.Settings.MultipleInstances=2
        $d.Settings.DisallowStartIfOnBatteries=$false;$d.Settings.StopIfGoingOnBatteries=$false
        $sddl='O:'+$r.sid+'D:P(A;;FA;;;'+$r.sid+')(A;;FA;;;SY)(A;;FA;;;BA)'
        $task=$folder.RegisterTaskDefinition($name,$d,2,$r.sid,$null,3,$sddl)
    }
    if ($r.action -eq 'start') {if (!$task){throw 'not_installed'};if($task.State -ne 4){$null=$task.Run($null)}}
    if ($r.action -eq 'uninstall' -and $task){$folder.DeleteTask($name,0);$task=$null}
    @{installed=($null -ne $task);running=$false;unsupported=$false;failed=$false;code='ok'}|ConvertTo-Json -Compress
} catch {
    @{installed=$false;running=$false;unsupported=$false;failed=$true;code='scheduler_failed_or_ownership_conflict'}|ConvertTo-Json -Compress
}
