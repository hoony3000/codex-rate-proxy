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

To keep it running after logout:

```bash
nohup ~/.local/bin/codex-rate-proxy > "$HOME/.config/codex-rate-proxy/proxy.log" 2>&1 &
```

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
