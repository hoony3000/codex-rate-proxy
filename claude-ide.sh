#!/bin/bash
# Use a claude-ide-<registered-name> symlink per user.
set +vx
set -eu

die() { printf 'claude-ide: %s\n' "$*" >&2; exit 1; }

launcher_name=${0##*/}
[[ "$launcher_name" == claude-ide-* ]] || die 'invoke as claude-ide-<registered-name>'
profile=${launcher_name#claude-ide-}
[[ "$profile" =~ ^[A-Za-z0-9_-]{1,64}$ ]] || die 'invalid registered name in launcher filename'

case ${ANTHROPIC_BASE_URL:-} in
    http://?*|https://?*) ;;
    *) die 'set ANTHROPIC_BASE_URL to the approved Anthropic Messages gateway' ;;
esac
case ${CLAUDE_IDE_AUTH_HEADER:-bearer} in
    bearer) key_variable=ANTHROPIC_AUTH_TOKEN ;;
    x-api-key) key_variable=ANTHROPIC_API_KEY ;;
    *) die 'CLAUDE_IDE_AUTH_HEADER must be bearer or x-api-key' ;;
esac

# The extension passes its bundled binary as argv[1] when present.
bundled=''
if [[ $# -gt 0 && "$1" == /* && -f "$1" && -x "$1" ]]; then
    bundled=$1
    shift
fi
claude_bin=${CLAUDE_IDE_BINARY:-${bundled:-"$HOME/.local/bin/claude"}}
[[ "$claude_bin" == /* && -f "$claude_bin" && -x "$claude_bin" ]] ||
    die 'Claude binary unavailable; set CLAUDE_IDE_BINARY to an installed absolute path'
[[ ! "$claude_bin" -ef "$0" ]] || die 'CLAUDE_IDE_BINARY must not point to this wrapper'

unset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN
key_setup=$("$HOME/.local/bin/codex-rate-proxy" env -u "$profile" --shell bash --var "$key_variable") ||
    die 'could not load the registered API key'
eval "$key_setup"
unset key_setup
[[ -n "${!key_variable:-}" ]] || die 'registered API key was empty'

exec "$claude_bin" "$@"
