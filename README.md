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
- Uses standard `HTTP_PROXY`, `HTTPS_PROXY`, and `NO_PROXY` variables
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

The only required variable is the actual upstream API base URL:

```bash
export LLM_TARGET_BASE_URL="https://llm.example.com/v1"
export NO_PROXY="localhost,127.0.0.1"
```

When a corporate forward proxy is required:

```bash
export HTTP_PROXY="http://proxy.example.com:8080"
export HTTPS_PROXY="http://proxy.example.com:8080"
export NO_PROXY="localhost,127.0.0.1"
```

Optional tuning:

```bash
export LLM_MIN_INTERVAL_SECONDS="10"
export LLM_MAX_RETRIES="5"
export LLM_BACKOFF_BASE_SECONDS="5"
export LLM_BACKOFF_MAX_SECONDS="60"
export LLM_BACKOFF_JITTER_SECONDS="1"
export LLM_UPSTREAM_TIMEOUT_SECONDS="600"
export LLM_PROXY_HOST="127.0.0.1"
export LLM_PROXY_PORT="8765"
```

For `csh` or `tcsh`, use `setenv`:

```csh
setenv LLM_TARGET_BASE_URL "https://llm.example.com/v1"
setenv NO_PROXY "localhost,127.0.0.1"
setenv LLM_MIN_INTERVAL_SECONDS "10"
```

## Run

```bash
python3 llm_rate_proxy.py
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
