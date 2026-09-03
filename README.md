# Codex Rate Proxy

A small local rate-limiting proxy for Codex CLI and OpenAI-compatible
Responses APIs. It uses only the Python standard library.

It is useful when an upstream API returns HTTP `429 Too Many Requests` because
Codex sends several model requests in a short period of time.

## Features

- Minimum interval between upstream request starts
- Exponential backoff for HTTP 429 responses
- Honors the upstream `Retry-After` header
- Streams SSE responses without changing their payload
- Reads runtime options from an INI configuration file
- Listens on `127.0.0.1` by default
- No `pip install` or root access required

## Requirements

- Python 3.8 or later
- An OpenAI-compatible API that supports `/v1/responses`

## Install

Copy `llm_rate_proxy.py` to the machine where Codex CLI runs:

```bash
chmod 700 llm_rate_proxy.py
```

## Configure

Copy the example configuration and edit the copy:

```bash
cp llm_rate_proxy.ini.example llm_rate_proxy.ini
```

```ini
[server]
host = 127.0.0.1
port = 8765

[upstream]
base_url = https://llm.example.com/v1
timeout_seconds = 600

[rate_limit]
min_interval_seconds = 10
max_retries = 5
backoff_base_seconds = 5
backoff_max_seconds = 60
backoff_jitter_seconds = 1

[forward_proxy]
http = http://proxy.example.com:8080
https = http://proxy.example.com:8080
bypass = localhost,127.0.0.1
```

Leave `http` and `https` empty when the upstream API is directly reachable.
The real `llm_rate_proxy.ini` is ignored by Git so that internal addresses or
proxy credentials are not committed.

## Run

```bash
python3 llm_rate_proxy.py
```

The default configuration path is `llm_rate_proxy.ini` beside the script. To
use a different file:

```bash
python3 llm_rate_proxy.py --config /path/to/proxy.ini
```

To keep it running after logout:

```bash
nohup python3 llm_rate_proxy.py > "$PWD/llm-rate-proxy.log" 2>&1 &
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
- TLS verification is performed by Python using the host's configured CA
  certificates.

## License

No license has been selected yet. Until a license is added, normal copyright
rules apply even if the repository is public.
