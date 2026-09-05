# Codex Rate Proxy

A small local rate-limiting proxy for Codex CLI and OpenAI-compatible
Responses APIs. The primary implementation is a static Rust binary for older
Linux systems such as CentOS 7.4. A Python standard-library implementation is
also included as a fallback.

It is useful when an upstream API returns HTTP `429 Too Many Requests` because
Codex sends several model requests in a short period of time.

## Features

- Minimum interval between upstream request starts
- Exponential backoff for HTTP 429 responses
- Honors the upstream `Retry-After` header
- Streams SSE responses without changing their payload
- Reads runtime options from an INI configuration file
- Listens on `127.0.0.1` by default
- Static `x86_64-unknown-linux-musl` release binary
- No Python, OpenSSL, libcurl, or root access required at runtime
- One-command Codex launch with per-key proxy reuse and automatic port allocation
- Idle cleanup with active Codex sessions and streaming requests protected

## Launch Codex (Linux, Bash and csh/tcsh)

The Rust `launch` command prepares or reuses a proxy and starts Codex for you.
Keep one shared INI at `~/.config/codex-rate-proxy/config.ini`. Define your
existing custom provider (default `corp`) and model in `~/.codex/config.toml`.
No per-user port or key-file path belongs in the shared INI.

```sh
# Uses this shell's OPENAI_API_KEY; asks with echo disabled if it is missing.
codex-rate-proxy launch

# Explicit key sources (choose one). Key files contain one line.
codex-rate-proxy launch --key-file /path/to/my-key
codex-rate-proxy launch --key-env MY_LLM_KEY
codex-rate-proxy launch --ask-key
key-producing-command | codex-rate-proxy launch --key-stdin

# Pass additional Codex arguments after --.
codex-rate-proxy launch -- --model YOUR_MODEL

# Prepare/reuse a proxy and print its URL without starting Codex.
codex-rate-proxy url --key-file /path/to/my-key
```

These commands have the same syntax in Bash and csh/tcsh; no `nohup` or PID
file commands are needed for managed mode. `launch` prints the local URL to
stderr; `url` prints only the URL to stdout. With `--key-stdin`, input must end
at EOF; Codex reconnects stdin to `/dev/tty` when a terminal is available.

The launcher passes the local `base_url`, provider selection, a dedicated
`CODEX_RATE_PROXY_API_KEY` environment variable and retry/transport overrides
to the Codex child. It never rewrites the shared Codex configuration or the
parent shell's environment. Existing `NO_PROXY` entries are retained and
loopback is added for the child. Forward-proxy settings for the Rust upstream
connection come exclusively from INI (ambient HTTP_PROXY is ignored).

| Situation | Result |
| --- | --- |
| Same key, upstream and canonical INI path | Reuse the running proxy |
| Concurrent launches with the same identity | Create only one proxy |
| Different key, upstream or canonical INI path | Create an independent proxy |
| Stale record and no daemon lifetime lock | Recreate with an OS-assigned port |
| Unresponsive process still holds its lock | Report an error; never blindly kill or duplicate it |
| Codex exits, proxy remains within idle timeout | Reuse on next launch |

The managed HTTP listener requires the matching Bearer key, including on
`/health`. Requests and 429 cooldown are shared by all sessions on that proxy.
Already-dispatched requests cannot be recalled when a new 429 arrives. This is
request pacing, not token counting or an exact TPM limiter. Separate Linux
accounts/HOME directories or different INI paths do not share limiter state.

### Cleanup

```sh
codex-rate-proxy list
codex-rate-proxy stop --key-file /path/to/my-key
codex-rate-proxy prune --dry-run
codex-rate-proxy prune
```

`stop` accepts the same key-source and `--config` options as `launch`, and
refuses to stop an instance with active sessions or requests. `list` and
`prune` cover managed instances under the current HOME, across all keys.
`prune` immediately stops unused instances; `--dry-run` only lists them.
Unresponsive instances are kept unless their lifetime lock proves they exited.
Legacy standalone proxies and unrelated processes are never targeted.

Automatic cleanup waits for no connected sessions AND no active requests,
streams or queued retries for `idle_timeout_seconds` (default 1800). Checks
run approximately every two seconds. Codex waiting for input or running tools
is still active. A kernel-backed lease is inherited by Codex, so killing its
launcher alone does not mark a surviving child unused. Descendants retaining
the lease also keep the proxy alive until they exit.

State lives in `~/.local/state/codex-rate-proxy/` with private directory
permissions. Each instance has a hashed identity, private control token,
local URL, PID and log; no API key or personal key-file path is saved there.
Records, control sockets and expired leases are removed on clean shutdown.
Small lock files and the last log remain intentionally to avoid lock-file
replacement races; logs are replaced when the same instance is recreated.
Runtime state should be on a filesystem with working Linux `flock` support.
The HOME path must fit a Unix socket pathname (108 bytes including the suffix).

Shared-account processes and files are not a security boundary between people
using the same Linux UID. API-key authentication prevents accidental cross-key
use; it does not isolate users sharing that UID. Key material is sent to the
daemon over a pipe and to Codex in its child environment, never as CLI arguments.

### Shared launcher policy

```ini
[launcher]
codex_binary = codex
provider = corp

[lifecycle]
idle_timeout_seconds = 1800
```

No `key_file`, personal paths or key-source preference is stored in INI.
An explicit key-source flag takes precedence over the default environment lookup.
Multiple key-source flags are rejected. Empty, multiline or malformed keys fail
before proxy creation. A managed proxy always binds loopback on an automatic
port; `[server] host/port` apply only to standalone mode.

SIGHUP still reloads valid policy changes for new requests; existing requests
keep their original policy. Changing the upstream of a managed proxy is rejected
on reload: launch again to create an instance with the new identity instead.
The Python fallback does not implement the Rust launcher/lifecycle features.

## Download a prebuilt binary

Download `codex-rate-proxy-x86_64-linux-musl.tar.gz` from the latest GitHub
Actions run or from a tagged GitHub Release. The musl binary is statically
linked and is intended to run on x86_64 CentOS 7.4 without a Rust or Python
installation.

```bash
tar -xzf codex-rate-proxy-x86_64-linux-musl.tar.gz
./install.sh
```

This installs the executable and initial configuration to:

```text
~/.local/bin/codex-rate-proxy
~/.config/codex-rate-proxy/config.ini
```

`install.sh` preserves an existing `config.ini`. No root access is required.
The release archive also contains the safe example configuration as
`config.ini`.

## Build from source

GNU build on the current Linux host:

```bash
cargo test
cargo build --release
```

Static musl build:

```bash
rustup target add x86_64-unknown-linux-musl
cargo test
cargo build --release --target x86_64-unknown-linux-musl
```

The output is:

```text
target/x86_64-unknown-linux-musl/release/codex-rate-proxy
```

To publish a release, push a version tag:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The GitHub workflow builds, verifies, packages, checksums, and attaches the
static binary to the release.

## Python fallback

Copy `llm_rate_proxy.py` to the machine where Codex CLI runs:

```bash
chmod 700 llm_rate_proxy.py
```

## Configure

Copy the example configuration and edit the copy:

```bash
mkdir -p ~/.config/codex-rate-proxy
cp llm_rate_proxy.ini.example ~/.config/codex-rate-proxy/config.ini
chmod 600 ~/.config/codex-rate-proxy/config.ini
vi ~/.config/codex-rate-proxy/config.ini
```

```ini
[server]
host = 127.0.0.1
port = 8765

[upstream]
base_url = http://llm.example.com/v1
timeout_seconds = 600
max_request_body_bytes = 134217728

[rate_limit]
min_interval_seconds = 10
max_retries = 5
backoff_base_seconds = 5
backoff_max_seconds = 60
backoff_jitter_seconds = 1

[forward_proxy]
http = http://proxy.example.com:8080
```

Leave `http` empty when the upstream API is directly reachable.
Do not commit the real `config.ini`, because it can contain internal addresses
or proxy credentials.

## Run

```bash
~/.local/bin/codex-rate-proxy
```

The default configuration path is
`~/.config/codex-rate-proxy/config.ini`. To use a different file:

```bash
~/.local/bin/codex-rate-proxy --config /path/to/proxy.ini
```

To run the Python fallback instead:

```bash
python3 llm_rate_proxy.py --config /path/to/proxy.ini
```

To keep it running after logout with Bash:

```bash
nohup ~/.local/bin/codex-rate-proxy > "$HOME/.config/codex-rate-proxy/proxy.log" 2>&1 &
echo $! > "$HOME/.config/codex-rate-proxy/proxy.pid"
```

With csh or tcsh, use `>>&` to append both standard output and standard error.
`>!` safely replaces an existing PID file even when `noclobber` is enabled:

```csh
nohup ~/.local/bin/codex-rate-proxy >>& ~/.config/codex-rate-proxy/proxy.log &
echo $! >! ~/.config/codex-rate-proxy/proxy.pid
```

After editing `config.ini`, reload it without interrupting the process.

Bash:

```bash
kill -HUP "$(cat "$HOME/.config/codex-rate-proxy/proxy.pid")"
```

csh or tcsh:

```csh
kill -HUP `cat ~/.config/codex-rate-proxy/proxy.pid`
```

The upstream address, timeout, request-size limit, rate-limit policy, and
forward proxy are applied to new requests. Existing requests and SSE streams
continue with their original settings. Changes to `[server] host` or `port`
require a restart. If the updated file is invalid, the proxy logs the error and
keeps the last valid configuration.

Health check:

```bash
curl http://127.0.0.1:8765/health
```

## Codex configuration

Point the provider in `~/.codex/config.toml` at the local proxy and disable
Codex-side retries so that this proxy owns the retry policy:

```toml
model = "YOUR_MODEL"
model_provider = "corp"

[model_providers.corp]
name = "Corporate LLM"
base_url = "http://127.0.0.1:8765/v1"
wire_api = "responses"
env_key = "OPENAI_API_KEY"
requires_openai_auth = false
request_max_retries = 0
stream_max_retries = 0
supports_websockets = false
```

Start a new Codex session after changing the configuration.

## Default retry policy

With the defaults, normal upstream request starts are at least 10 seconds
apart. A 429 response is retried after approximately 5, 10, 20, 40, and 60
seconds. If `Retry-After` requests a longer delay, that delay wins.

The request JSON and response body are not modified. The proxy only controls
when requests are sent and relays the final response.

## Security notes

- Keep the default `127.0.0.1` listener unless remote access is intentional.
- Do not put API keys or internal hostnames in this repository.
- Prompts and response bodies are not written to the proxy log.
- The Rust implementation intentionally supports only an HTTP upstream and an
  optional HTTP forward proxy. It contains no TLS stack.

## License

No license has been selected yet. Until a license is added, normal copyright
rules apply even if the repository is public.
