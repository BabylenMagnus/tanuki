# installed by tanuki
# managed by tanuki; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# TANUKI_INTEGRATION_ID=kimi
# TANUKI_INTEGRATION_VERSION=5

param([string]$Action = "")

if (@("session", "working", "blocked", "idle") -notcontains $Action) { exit 0 }
if ($env:TANUKI_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:TANUKI_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = $null
}

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$sessionId = if ($null -ne $payload -and -not [string]::IsNullOrWhiteSpace($payload.session_id)) { $payload.session_id } else { $null }

try {
    if ($Action -eq "session") {
        if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }
        & tanuki pane report-agent-session $env:TANUKI_PANE_ID --source tanuki:kimi --agent kimi --agent-session-id $sessionId --seq $seq 2>$null | Out-Null
    } else {
        if ([string]::IsNullOrWhiteSpace($sessionId)) {
            & tanuki pane report-agent $env:TANUKI_PANE_ID --source tanuki:kimi --agent kimi --state $Action --seq $seq 2>$null | Out-Null
        } else {
            & tanuki pane report-agent $env:TANUKI_PANE_ID --source tanuki:kimi --agent kimi --state $Action --agent-session-id $sessionId --seq $seq 2>$null | Out-Null
        }
    }
} catch {
}
