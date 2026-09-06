# HumCon — PowerShell command log hook.
#
# Appends one JSON object per interactive command to $HUMCON_DIR/commands.jsonl,
# matching snapshot.json's `recent_commands` entry shape exactly:
#
#     {"command":"git status","ran_at":"2026-08-29T04:12:07Z"}
#
# Install: sourced from $PROFILE:
#     . C:\Users\<you>\HumCon\hooks\humcon-log.ps1
# Uninstall: remove that line from $PROFILE

if (-not $script:HumconLogInitialized) {
    $script:HumconLogInitialized = $true
    $script:HumconLastCommandId = $null

    $script:HumconDir = if ($env:HUMCON_DIR) {
        $env:HUMCON_DIR
    } else {
        Join-Path ([Environment]::GetFolderPath("UserProfile")) ".humcon"
    }

    $script:HumconCmdLog = Join-Path $script:HumconDir "commands.jsonl"
    $script:HumconSecretRegex = 'password|passwd|passphrase|secret|token|api[_-]?key|apikey|bearer|credential|private[_-]?key|--password='

    function global:__humcon_log_command {
        try {
            $last = Get-History -Count 1 -ErrorAction SilentlyContinue
            if (-not $last) { return }

            if ($null -eq $script:HumconLastCommandId) {
                # First run in this session: prime baseline ID so we don't log prior session commands
                $script:HumconLastCommandId = $last.Id
                return
            }

            if ($last.Id -eq $script:HumconLastCommandId) {
                return
            }
            $script:HumconLastCommandId = $last.Id

            $cmd = $last.CommandLine
            if ([string]::IsNullOrWhiteSpace($cmd)) { return }

            if ($cmd -match $script:HumconSecretRegex) { return }

            $ts = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
            $esc = $cmd.Replace("\", "\\").Replace('"', '\"').Replace("`t", " ").Replace("`r", " ").Replace("`n", " ")
            $json = "{`"command`":`"$esc`",`"ran_at`":`"$ts`"}"

            if (-not (Test-Path $script:HumconDir)) {
                New-Item -ItemType Directory -Path $script:HumconDir -Force | Out-Null
            }

            [System.IO.File]::AppendAllText($script:HumconCmdLog, "$json`n", [System.Text.Encoding]::UTF8)
        } catch {
            # Never throw or disrupt the user's interactive shell prompt
        }
    }

    if (Test-Path Function:\prompt) {
        $script:__humcon_orig_prompt = Get-Command prompt
        function global:prompt {
            __humcon_log_command
            & $script:__humcon_orig_prompt
        }
    } else {
        function global:prompt {
            __humcon_log_command
            "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) "
        }
    }
}
