# HumCon — shell command log hook.
#
# Appends one JSON object per interactive command to $HUMCON_DIR/commands.jsonl,
# matching snapshot.json's `recent_commands` entry shape exactly:
#
#     {"command":"git status","ran_at":"2026-08-29T04:12:07Z"}
#
# It deliberately does NOT write snapshot.json. Every snapshot write goes
# through a single Mutex<Snapshot> inside the Tauri process (architecture.md,
# Session 0). This hook is a *separate process*, so that mutex cannot protect
# it, and the window-capture poller rewrites the whole file every 3 seconds — a
# read-modify-write from here would clobber active_window. The Tauri side tails
# this log instead and merges it into recent_commands.
#
# Install:   sourced from ~/.bashrc
# Uninstall: remove that source line

# Interactive shells only. PROMPT_COMMAND never fires anywhere else, which also
# means commands run by non-interactive tooling are not captured (documented in
# architecture.md as a deliberate boundary, not an oversight).
[[ $- != *i* ]] && return

# Windows-visible path: the Tauri app is a native Windows process and cannot
# see WSL-only paths (CLAUDE.md). Override HUMCON_DIR to relocate.
: "${HUMCON_DIR:=/mnt/c/Users/Vaishnav/.humcon}"
HUMCON_CMD_LOG="$HUMCON_DIR/commands.jsonl"

# The lock deliberately lives in WSL rather than on /mnt/c: flock semantics on
# the drvfs mount are unreliable, and every interactive WSL shell shares /tmp,
# which is all this lock needs to coordinate. The Windows app never reads it.
HUMCON_CMD_LOCK="/tmp/humcon-commands.lock"

HUMCON_LOG_MAX_BYTES=65536
HUMCON_LOG_KEEP_LINES=100

# Commands matching this are skipped entirely rather than redacted, per the
# project constraint. Over-matching is intentional: dropping `man passwd` costs
# nothing, while a secret that slips through cannot be un-leaked. Matched
# against a lowercased copy of the line, so the pattern is all lowercase.
HUMCON_SECRET_RE='password|passwd|passphrase|secret|token|api[_-]?key|apikey|bearer|credential|private[_-]?key|--password=|ssh-add|(^| )gpg( |$)|-u *[^ ]+:[^ ]+|[a-z_]*(key|token|secret|pass)[a-z_]*='

# Trim leading whitespace, then split "  423  git status" into index + command.
__humcon_split_history() {
    local raw=$1
    raw="${raw#"${raw%%[![:space:]]*}"}"
    __humcon_histnum="${raw%%[[:space:]]*}"
    __humcon_histcmd="${raw#"$__humcon_histnum"}"
    __humcon_histcmd="${__humcon_histcmd#"${__humcon_histcmd%%[![:space:]]*}"}"
}

__humcon_append() {
    local line=$1
    mkdir -p "$HUMCON_DIR" 2>/dev/null || return 0

    # Subshell so `exit` on a failed lock cannot kill the user's shell.
    (
        flock -w 1 9 || exit 0
        printf '%s\n' "$line" >> "$HUMCON_CMD_LOG"

        # Keep the raw log bounded. The Rust side only ever needs the last 20,
        # so this just stops unbounded growth; trimming happens rarely and only
        # while the lock is held.
        local size
        size=$(stat -c %s "$HUMCON_CMD_LOG" 2>/dev/null || echo 0)
        if (( size > HUMCON_LOG_MAX_BYTES )); then
            tail -n "$HUMCON_LOG_KEEP_LINES" "$HUMCON_CMD_LOG" > "$HUMCON_CMD_LOG.tmp" \
                && mv -f "$HUMCON_CMD_LOG.tmp" "$HUMCON_CMD_LOG"
        fi
    ) 9>"$HUMCON_CMD_LOCK" 2>/dev/null

    return 0
}

__humcon_log_command() {
    local cmd ts esc

    # `HISTTIMEFORMAT=` pins the output shape, so setting a history time format
    # later cannot corrupt the parse. `builtin` avoids any alias or function
    # shadowing `history`.
    __humcon_split_history "$(HISTTIMEFORMAT= builtin history 1 2>/dev/null)"
    cmd=$__humcon_histcmd

    # PROMPT_COMMAND also fires before the *first* prompt of a shell, when no
    # command has run yet in this session. At that point `history 1` returns the
    # last command of the *previous* session, loaded from HISTFILE — logging it
    # would put a stale command at the front of the log. So the first firing
    # only records a baseline index. (Seeding this when the file is sourced does
    # not work: bash loads HISTFILE *after* running the rc file, so the history
    # list is still empty then.)
    if [[ -z ${HUMCON_PRIMED:-} ]]; then
        HUMCON_PRIMED=1
        HUMCON_LAST_HISTNUM=$__humcon_histnum
        return 0
    fi

    [[ -z $cmd ]] && return 0

    # PROMPT_COMMAND fires on *every* prompt, including a bare Enter. Without
    # this index check, pressing Enter on an empty line would re-log the
    # previous command over and over.
    [[ $__humcon_histnum == "$HUMCON_LAST_HISTNUM" ]] && return 0
    HUMCON_LAST_HISTNUM=$__humcon_histnum

    # Skip, never redact.
    [[ ${cmd,,} =~ $HUMCON_SECRET_RE ]] && return 0

    # Matches Rust's to_rfc3339_opts(SecondsFormat::Secs, true) byte for byte.
    ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    # JSON-escape without spawning a process (no jq here, and this runs on every
    # prompt). Backslashes must go first, or they would double-escape the quote
    # escapes added next. Raw UTF-8 passes through, which JSON permits.
    esc=${cmd//\\/\\\\}
    esc=${esc//\"/\\\"}
    esc=${esc//$'\t'/ }
    esc=${esc//$'\n'/ }
    esc=${esc//$'\r'/ }

    __humcon_append "{\"command\":\"$esc\",\"ran_at\":\"$ts\"}"
}

# Deliberately not seeded here — see __humcon_log_command's priming comment.
HUMCON_LAST_HISTNUM=""
HUMCON_PRIMED=""

# Chain rather than clobber, so this coexists with anything else already hooked.
if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) =~ "declare -a" ]]; then
    if [[ ! " ${PROMPT_COMMAND[*]} " =~ " __humcon_log_command " ]]; then
        PROMPT_COMMAND+=(__humcon_log_command)
    fi
else
    if [[ -z ${PROMPT_COMMAND:-} ]]; then
        PROMPT_COMMAND="__humcon_log_command"
    elif [[ $PROMPT_COMMAND != *__humcon_log_command* ]]; then
        PROMPT_COMMAND="${PROMPT_COMMAND%;};__humcon_log_command"
    fi
fi
