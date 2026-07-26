# installed by tanuki
# managed by tanuki; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# TANUKI_INTEGRATION_ID=copilot
# TANUKI_INTEGRATION_VERSION=2

if ($env:TANUKI_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:TANUKI_PANE_ID)) { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:TANUKI_SOCKET_PATH)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { @{} } else { $inputText | ConvertFrom-Json }
} catch {
    $payload = @{}
}

function First-Text {
    param([object[]]$Names)
    foreach ($name in $Names) {
        $value = $payload.$name
        if ($value -is [string] -and -not [string]::IsNullOrWhiteSpace($value)) {
            return $value
        }
    }
    return $null
}

function Normalize-Event {
    param([string]$Event)
    if ([string]::IsNullOrWhiteSpace($Event)) { return "" }
    return $Event.Replace("_", "").Replace("-", "").ToLowerInvariant()
}

$eventName = First-Text @("hook_event_name", "hookEventName")
if ($eventName) {
    if ((Normalize-Event $eventName) -ne "sessionstart") { exit 0 }
} elseif (
    ($payload.PSObject.Properties.Name -contains "prompt") -or
    (First-Text @("tool_name", "toolName", "notification_type", "notificationType", "stop_reason", "stopReason", "reason"))
) {
    exit 0
}

$sessionId = $payload.session_id
if ([string]::IsNullOrWhiteSpace($sessionId)) {
    $sessionId = $payload.sessionId
}
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
& tanuki pane report-agent-session $env:TANUKI_PANE_ID --source tanuki:copilot --agent copilot --agent-session-id $sessionId --seq $seq 2>$null | Out-Null
