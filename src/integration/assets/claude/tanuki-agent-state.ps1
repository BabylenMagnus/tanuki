# installed by tanuki
# managed by tanuki; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# TANUKI_INTEGRATION_ID=claude
# TANUKI_INTEGRATION_VERSION=7

param([string]$Action = "")

if ($Action -ne "session") { exit 0 }
if ($env:TANUKI_ENV -ne "1") { exit 0 }
if ([string]::IsNullOrWhiteSpace($env:TANUKI_PANE_ID)) { exit 0 }

$inputText = [Console]::In.ReadToEnd()
try {
    $payload = if ([string]::IsNullOrWhiteSpace($inputText)) { $null } else { $inputText | ConvertFrom-Json }
} catch {
    exit 0
}

if (-not [string]::IsNullOrWhiteSpace($payload.agent_id)) { exit 0 }
if ($payload.hook_event_name -eq "SubagentStop") { exit 0 }

$sessionId = $payload.session_id
if ([string]::IsNullOrWhiteSpace($sessionId)) { exit 0 }

$seq = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
try {
    $args = @(
        "pane",
        "report-agent-session",
        $env:TANUKI_PANE_ID,
        "--source",
        "tanuki:claude",
        "--agent",
        "claude",
        "--seq",
        "$seq",
        "--agent-session-id",
        "$sessionId"
    )
    if ($payload.transcript_path -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.transcript_path)) {
        $args += @("--agent-session-path", "$($payload.transcript_path)")
    }
    if ($payload.hook_event_name -eq "SessionStart" -and $payload.source -is [string] -and -not [string]::IsNullOrWhiteSpace($payload.source)) {
        $args += @("--session-start-source", "$($payload.source)")
    }
    & tanuki @args 2>$null | Out-Null
} catch {
}
