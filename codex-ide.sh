#!/bin/bash
# Install as ~/.local/bin/codex-ide-<registered-name>. Requires Bash and coreutils.
set +vx
set -eu
shopt -s nullglob

die() { printf 'codex-ide: %s\n' "$*" >&2; exit 1; }

# Limit the search to installed OpenAI Codex extensions on this x86_64 server.
resolve_codex() {
    local root ext name selected='' version selected_version=''
    for root in "$@"; do
        [[ -d "$root" ]] || continue
        for ext in "$root"/openai.chatgpt-*; do
            name=${ext##*/}
            [[ "$name" =~ ^openai[.]chatgpt-([0-9]+([.][0-9]+)*)(-linux-x64)?$ ]] || continue
            version=${BASH_REMATCH[1]}
            local candidate="$ext/bin/linux-x86_64/codex"
            [[ -f "$candidate" && -x "$candidate" ]] || continue
            if [[ -z "$selected" ]] ||
                [[ "$(printf '%s\n' "$selected_version" "$version" | LC_ALL=C sort -V | tail -n 1)" == "$version" ]]; then
                selected=$candidate
                selected_version=$version
            fi
        done
        if [[ -n "$selected" ]]; then
            printf '%s\n' "$selected"
            return
        fi
    done
    return 1
}

[[ "$(uname -m)" == x86_64 ]] || die 'this launcher expects an x86_64 Linux server'
if [[ -n "${CODEX_IDE_EXTENSIONS_DIR:-}" ]]; then
    roots=("$CODEX_IDE_EXTENSIONS_DIR")
elif [[ -n "${VSCODE_AGENT_FOLDER:-}" ]]; then
    roots=("$VSCODE_AGENT_FOLDER/extensions" "$HOME/.vscode-server/extensions")
else
    roots=("$HOME/.vscode-server/extensions" "$HOME/.vscode-server-insiders/extensions")
fi

codex_bin=$(resolve_codex "${roots[@]}") ||
    die 'bundled Codex not found; check that OpenAI Codex is installed on the remote host'

if [[ $# -eq 1 && "$1" == --resolve-codex ]]; then
    printf '%s\n' "$codex_bin"
    exit 0
fi

launcher_name=${0##*/}
[[ "$launcher_name" == codex-ide-* ]] || die 'install this file as codex-ide-<registered-name>'
profile=${launcher_name#codex-ide-}
[[ "$profile" =~ ^[A-Za-z0-9_-]+$ ]] || die 'invalid registered name in launcher filename'

unset OPENAI_API_KEY
key_setup=$("$HOME/.local/bin/codex-rate-proxy" env -u "$profile" --shell bash) ||
    die 'could not load the registered API key'
eval "$key_setup"
unset key_setup
[[ -n "${OPENAI_API_KEY:-}" ]] || die 'registered API key was empty'

exec "$codex_bin" "$@"
