# pkgtrack bash hook — eval "$(pkgtrack init bash)" in ~/.bashrc
# This hook uses PROMPT_COMMAND to log the most recent command.
# It is idempotent: sourcing .bashrc twice won't double-log.

# Guard: only run in interactive shells, skip if already loaded
if [[ "$-" != *i* ]] || [[ -n "$__PKGTRACK_LOADED" ]]; then
    return 0 2>/dev/null || true
fi
__PKGTRACK_LOADED=1

# Resolve paths once
__PKGTRACK_LOG="${XDG_DATA_HOME:-$HOME/.local/share}/pkgtrack/events.log"
# Initialize with current HISTCMD so the initial prompt display isn't logged
__PKGTRACK_LAST_HISTCMD="$HISTCMD"

__pkgtrack_hook() {
    # Get the current history number; skip if unchanged (empty Enter)
    local _histcmd
    _histcmd=$HISTCMD
    if [[ "$_histcmd" == "$__PKGTRACK_LAST_HISTCMD" ]]; then
        return
    fi
    __PKGTRACK_LAST_HISTCMD="$_histcmd"

    # Get the last command from history
    local _raw _num _lastcmd
    _raw=$(HISTTIMEFORMAT='' builtin history 1)
    read -r _num _lastcmd <<< "$_raw"

    # Skip empty commands
    [[ -z "$_lastcmd" ]] && return

    # Extract epoch seconds using builtin
    local _epoch
    printf -v _epoch '%(%s)T' -1

    # Split on ;, &&, ||, | and log each segment's first real command
    local _segments _seg _word _skip_next
    _segments="${_lastcmd//&&/$'\n'}"
    _segments="${_segments//||/$'\n'}"
    _segments="${_segments//|/$'\n'}"
    _segments="${_segments//;/$'\n'}"

    while IFS= read -r _seg; do
        # Trim leading whitespace
        _seg="${_seg#"${_seg%%[![:space:]]*}"}"
        [[ -z "$_seg" ]] && continue

        _word=""
        _skip_next=0
        for _token in $_seg; do
            if [[ "$_skip_next" -eq 1 ]]; then
                _skip_next=0
                continue
            fi

            case "$_token" in
                *=*) # Check if it's a valid VAR=value assignment
                    case "${_token%%=*}" in
                        [a-zA-Z_]*) continue ;;
                        *) _word="$_token"; break ;;
                    esac
                    ;;
                sudo|doas|env|time|nohup|exec|command|builtin)
                    continue ;;
                -u|-g|-C)
                    _skip_next=1
                    continue ;;
                -*)
                    continue ;;
                *)
                    _word="$_token"
                    break
                    ;;
            esac
        done

        [[ -z "$_word" ]] && continue

        # Extract basename if it's a path
        _word="${_word##*/}"

        # Ensure log directory exists
        [[ -d "${__PKGTRACK_LOG%/*}" ]] || mkdir -p "${__PKGTRACK_LOG%/*}" 2>/dev/null

        # Append to log (single write, atomic for short lines)
        printf '%s shell %s\n' "$_epoch" "$_word" >> "$__PKGTRACK_LOG"
    done <<< "$_segments"
}

# Append to PROMPT_COMMAND without overwriting existing entries
if [[ -z "$PROMPT_COMMAND" ]]; then
    PROMPT_COMMAND="__pkgtrack_hook"
elif [[ "$PROMPT_COMMAND" != *"__pkgtrack_hook"* ]]; then
    PROMPT_COMMAND="__pkgtrack_hook;$PROMPT_COMMAND"
fi
