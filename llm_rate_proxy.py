#!/usr/bin/env python3
"""Local rate-limit and retry proxy for an OpenAI-compatible API.

Uses only the Python standard library. Designed for Codex CLI running on a
remote CentOS host behind an HTTP/HTTPS corporate proxy.
"""

from __future__ import annotations

import email.utils
import argparse
import configparser
import http.server
import os
import random
import socketserver
import sys
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from typing import BinaryIO, Optional


SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
DEFAULT_CONFIG_PATH = os.path.join(SCRIPT_DIR, "llm_rate_proxy.ini")

# Populated by load_config() before the server starts.
LISTEN_HOST = "127.0.0.1"
LISTEN_PORT = 8765
TARGET_BASE_URL = ""
MIN_INTERVAL = 10.0
MAX_RETRIES = 5
BACKOFF_BASE = 5.0
BACKOFF_MAX = 60.0
JITTER = 1.0
UPSTREAM_TIMEOUT = 600.0
FORWARD_PROXIES = {}
NO_PROXY = "localhost,127.0.0.1"

HOP_BY_HOP_HEADERS = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
}


class RequestPacer:
    """Serializes upstream starts and guarantees a minimum start interval."""

    def __init__(self, minimum_interval: float) -> None:
        self.minimum_interval = max(0.0, minimum_interval)
        self._lock = threading.Lock()
        self._last_start = 0.0

    def wait_turn(self) -> None:
        with self._lock:
            now = time.monotonic()
            delay = self.minimum_interval - (now - self._last_start)
            if delay > 0:
                log("rate limit: waiting %.1f seconds" % delay)
                time.sleep(delay)
            self._last_start = time.monotonic()


PACER = RequestPacer(MIN_INTERVAL)


def load_config(path: str) -> None:
    global LISTEN_HOST, LISTEN_PORT, TARGET_BASE_URL
    global MIN_INTERVAL, MAX_RETRIES, BACKOFF_BASE, BACKOFF_MAX, JITTER
    global UPSTREAM_TIMEOUT, FORWARD_PROXIES, NO_PROXY, PACER

    parser = configparser.ConfigParser(interpolation=None)
    try:
        with open(path, "r", encoding="utf-8") as config_file:
            parser.read_file(config_file)
    except FileNotFoundError:
        raise ValueError("config file not found: %s" % path)
    except (OSError, configparser.Error) as exc:
        raise ValueError("could not read config file %s: %s" % (path, exc))

    try:
        LISTEN_HOST = parser.get("server", "host", fallback="127.0.0.1").strip()
        LISTEN_PORT = parser.getint("server", "port", fallback=8765)
        TARGET_BASE_URL = parser.get("upstream", "base_url").strip().rstrip("/")
        UPSTREAM_TIMEOUT = parser.getfloat(
            "upstream", "timeout_seconds", fallback=600.0
        )
        MIN_INTERVAL = parser.getfloat(
            "rate_limit", "min_interval_seconds", fallback=10.0
        )
        MAX_RETRIES = parser.getint("rate_limit", "max_retries", fallback=5)
        BACKOFF_BASE = parser.getfloat(
            "rate_limit", "backoff_base_seconds", fallback=5.0
        )
        BACKOFF_MAX = parser.getfloat(
            "rate_limit", "backoff_max_seconds", fallback=60.0
        )
        JITTER = parser.getfloat(
            "rate_limit", "backoff_jitter_seconds", fallback=1.0
        )
        http_proxy = parser.get("forward_proxy", "http", fallback="").strip()
        https_proxy = parser.get("forward_proxy", "https", fallback="").strip()
        NO_PROXY = parser.get(
            "forward_proxy", "bypass", fallback="localhost,127.0.0.1"
        ).strip()
    except (configparser.Error, ValueError) as exc:
        raise ValueError("invalid config value in %s: %s" % (path, exc))

    if not LISTEN_HOST:
        raise ValueError("[server] host must not be empty")
    if not 1 <= LISTEN_PORT <= 65535:
        raise ValueError("[server] port must be between 1 and 65535")
    if not TARGET_BASE_URL.startswith(("http://", "https://")):
        raise ValueError("[upstream] base_url must start with http:// or https://")
    if min(UPSTREAM_TIMEOUT, MIN_INTERVAL, BACKOFF_BASE, BACKOFF_MAX, JITTER) < 0:
        raise ValueError("timeout, interval, backoff, and jitter values must be non-negative")
    if MAX_RETRIES < 0:
        raise ValueError("[rate_limit] max_retries must be non-negative")

    FORWARD_PROXIES = {}
    if http_proxy:
        FORWARD_PROXIES["http"] = http_proxy
    if https_proxy:
        FORWARD_PROXIES["https"] = https_proxy

    # urllib's bypass check reads no_proxy. Set it only inside this process from
    # the INI file so users do not need to export an environment variable.
    if NO_PROXY:
        os.environ["no_proxy"] = NO_PROXY

    PACER = RequestPacer(MIN_INTERVAL)


def log(message: str) -> None:
    timestamp = time.strftime("%Y-%m-%d %H:%M:%S")
    print("[%s] %s" % (timestamp, message), file=sys.stderr, flush=True)


def retry_after_seconds(value: Optional[str]) -> Optional[float]:
    if not value:
        return None
    try:
        return max(0.0, float(value.strip()))
    except ValueError:
        pass
    try:
        retry_at = email.utils.parsedate_to_datetime(value)
        if retry_at.tzinfo is None:
            retry_at = retry_at.replace(tzinfo=timezone.utc)
        return max(0.0, (retry_at - datetime.now(timezone.utc)).total_seconds())
    except (TypeError, ValueError, OverflowError):
        return None


def backoff_seconds(attempt: int, retry_after: Optional[str]) -> float:
    calculated = min(BACKOFF_MAX, BACKOFF_BASE * (2 ** attempt))
    server_delay = retry_after_seconds(retry_after) or 0.0
    jitter = random.uniform(0.0, max(0.0, JITTER))
    return max(calculated, server_delay) + jitter


def copy_stream(source: BinaryIO, destination: BinaryIO) -> None:
    while True:
        chunk = source.read(64 * 1024)
        if not chunk:
            break
        destination.write(chunk)
        destination.flush()


class ThreadingHTTPServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class ProxyHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "LLMRateProxy/1.0"

    def do_POST(self) -> None:
        self._proxy_request()

    def do_GET(self) -> None:
        if self.path == "/health":
            body = b'{"status":"ok"}\n'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)
            self.close_connection = True
            return
        self._proxy_request()

    def do_DELETE(self) -> None:
        self._proxy_request()

    def do_PUT(self) -> None:
        self._proxy_request()

    def do_PATCH(self) -> None:
        self._proxy_request()

    def _read_body(self) -> bytes:
        transfer_encoding = self.headers.get("Transfer-Encoding", "").lower()
        if "chunked" in transfer_encoding:
            chunks = []
            while True:
                line = self.rfile.readline().strip()
                size = int(line.split(b";", 1)[0], 16)
                if size == 0:
                    while self.rfile.readline() not in (b"\r\n", b"\n", b""):
                        pass
                    break
                chunks.append(self.rfile.read(size))
                self.rfile.read(2)
            return b"".join(chunks)
        length = int(self.headers.get("Content-Length", "0"))
        return self.rfile.read(length) if length else b""

    def _target_url(self) -> str:
        # TARGET_BASE_URL normally ends in /v1, while Codex sends /v1/responses.
        if TARGET_BASE_URL.endswith("/v1") and self.path.startswith("/v1/"):
            return TARGET_BASE_URL + self.path[3:]
        return TARGET_BASE_URL + (self.path if self.path.startswith("/") else "/" + self.path)

    def _upstream_request(self, body: bytes) -> urllib.request.Request:
        headers = {}
        for name, value in self.headers.items():
            lower = name.lower()
            if lower not in HOP_BY_HOP_HEADERS and lower not in {"host", "content-length"}:
                headers[name] = value
        return urllib.request.Request(
            self._target_url(), data=body if body else None, headers=headers, method=self.command
        )

    def _proxy_request(self) -> None:
        try:
            body = self._read_body()
        except (ValueError, OSError) as exc:
            self.send_error(400, "Could not read request body: %s" % exc)
            return

        for attempt in range(MAX_RETRIES + 1):
            PACER.wait_turn()
            log("%s %s -> upstream (attempt %d/%d)" % (
                self.command, self.path, attempt + 1, MAX_RETRIES + 1
            ))
            try:
                response = urllib.request.urlopen(
                    self._upstream_request(body), timeout=UPSTREAM_TIMEOUT
                )
                self._relay(response)
                return
            except urllib.error.HTTPError as exc:
                if exc.code == 429 and attempt < MAX_RETRIES:
                    delay = backoff_seconds(attempt, exc.headers.get("Retry-After"))
                    exc.close()
                    log("upstream returned 429; retrying in %.1f seconds" % delay)
                    time.sleep(delay)
                    continue
                self._relay(exc)
                return
            except (urllib.error.URLError, TimeoutError, OSError) as exc:
                log("upstream connection failed: %s" % exc)
                self.send_error(502, "Upstream connection failed")
                return

    def _relay(self, response: BinaryIO) -> None:
        try:
            status = getattr(response, "status", getattr(response, "code", 502))
            reason = getattr(response, "reason", None)
            self.send_response(status, reason)
            for name, value in response.headers.items():
                if name.lower() not in HOP_BY_HOP_HEADERS and name.lower() != "connection":
                    self.send_header(name, value)
            self.send_header("Connection", "close")
            self.end_headers()
            copy_stream(response, self.wfile)
            log("upstream completed with HTTP %s" % status)
        except (BrokenPipeError, ConnectionResetError):
            log("client disconnected while response was streaming")
        finally:
            response.close()
            self.close_connection = True

    def log_message(self, fmt: str, *args: object) -> None:
        log("client %s - %s" % (self.client_address[0], fmt % args))


def main() -> None:
    argument_parser = argparse.ArgumentParser(description=__doc__)
    argument_parser.add_argument(
        "-c",
        "--config",
        default=DEFAULT_CONFIG_PATH,
        help="INI config path (default: %(default)s)",
    )
    args = argument_parser.parse_args()

    try:
        load_config(os.path.abspath(os.path.expanduser(args.config)))
    except ValueError as exc:
        print("configuration error: %s" % exc, file=sys.stderr)
        raise SystemExit(2)

    urllib.request.install_opener(
        urllib.request.build_opener(urllib.request.ProxyHandler(FORWARD_PROXIES))
    )
    server = ThreadingHTTPServer((LISTEN_HOST, LISTEN_PORT), ProxyHandler)
    log("listening on http://%s:%d" % (LISTEN_HOST, LISTEN_PORT))
    log("target: %s" % TARGET_BASE_URL)
    log("forward proxy: %s" % ("configured" if FORWARD_PROXIES else "direct"))
    log("minimum interval: %.1fs; 429 retries: %d" % (MIN_INTERVAL, MAX_RETRIES))
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        log("stopping")
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
