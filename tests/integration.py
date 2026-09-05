"""Offline integration tests against the built Linux binary; no real API keys or Codex needed."""
import concurrent.futures
import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request

BIN = str(Path(sys.argv.pop(1)).resolve())
KEY_A = "fake-integration-key-a"
KEY_B = "fake-integration-key-b"
STREAM_RELEASE = threading.Event()
RETRY_SEEN = set()
RETRY_LOCK = threading.Lock()


class Upstream(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if self.path.endswith("/retry"):
            with RETRY_LOCK:
                first = body not in RETRY_SEEN
                RETRY_SEEN.add(body)
            if first:
                self.send_response(429)
                self.send_header("Retry-After", "1")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
        if self.path.endswith("/limited"):
            self.send_response(429)
            self.send_header("Retry-After", "1")
            self.send_header("Content-Length", "0")
            self.end_headers()
        elif self.path.endswith("/stream"):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(b'data: {"part":1}\n\n')
            self.wfile.flush()
            STREAM_RELEASE.wait(10)
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
            self.close_connection = True
        else:
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    def log_message(self, *args):
        pass


def wait_until(check, timeout=10):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if check():
            return
        time.sleep(0.1)
    raise AssertionError("condition did not become true")


class Integration(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.api = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
        threading.Thread(target=cls.api.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.api.shutdown()
        cls.api.server_close()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="crp-", dir="/tmp")
        self.home = Path(self.temp.name)
        self.env = dict(os.environ, HOME=str(self.home), OPENAI_API_KEY=KEY_A)
        # A bogus inherited corporate proxy must not redirect direct local test traffic.
        self.env.update(HTTP_PROXY="http://127.0.0.1:1", HTTPS_PROXY="http://127.0.0.1:1")
        self.config = self.home / "config.ini"
        self.mock = self.home / "codex"
        self.mock.write_text("#!/usr/bin/env python3\n" +
            "import json, os, pathlib, sys, time\n" +
            "pathlib.Path(os.environ['MOCK_REPORT']).write_text(json.dumps({'args':sys.argv[1:], 'pid':os.getpid(), 'key_ok':os.environ.get('CODEX_RATE_PROXY_API_KEY') == 'fake-integration-key-a'}))\n" +
            "while not pathlib.Path(os.environ['MOCK_RELEASE']).exists(): time.sleep(0.1)\n")
        self.mock.chmod(0o700)
        self.write_config()
        self.processes = []
        STREAM_RELEASE.clear()

    def write_config(self, idle=30, interval=0.1, retries=0):
        self.config.write_text(f"""[upstream]
base_url = http://127.0.0.1:{self.api.server_port}/v1
timeout_seconds = 15
[rate_limit]
min_interval_seconds = {interval}
max_retries = {retries}
backoff_base_seconds = 0.2
backoff_max_seconds = 1
backoff_jitter_seconds = 0
[forward_proxy]
http =
[launcher]
codex_binary = {self.mock}
provider = corp
[lifecycle]
idle_timeout_seconds = {idle}
""")

    def cli(self, *args, env=None, check=True, input=None):
        return subprocess.run([BIN, *args], env=env or self.env, text=True,
            input=input, capture_output=True, timeout=25, check=check)

    def url(self, key=KEY_A):
        return self.cli("url", "--config", str(self.config),
            env=dict(self.env, OPENAI_API_KEY=key)).stdout.strip()

    def records(self):
        return list(self.home.glob(".local/state/codex-rate-proxy/*/record.json"))

    def request(self, url, key=KEY_A, path="/responses", data=b'{"hello":"world"}'):
        request = urllib.request.Request(url + path, data=data,
            headers={"Authorization": "Bearer " + key})
        return urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=15)

    def tearDown(self):
        STREAM_RELEASE.set()
        (self.home / "release").touch()
        for proc in self.processes:
            if proc.poll() is None:
                proc.terminate()
            proc.wait(timeout=10)
        self.cli("prune", check=False)
        try:
            wait_until(lambda: not self.records(), timeout=6)
        finally:
            # Tests own these daemon PIDs and this temporary HOME; no production processes.
            for path in self.records():
                record = json.loads(path.read_text())
                try:
                    os.kill(record["pid"], signal.SIGKILL)
                except ProcessLookupError:
                    pass
            self.temp.cleanup()

    def test_reuse_concurrency_and_key_isolation(self):
        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
            urls = list(pool.map(lambda _: self.url(), range(6)))
        self.assertEqual(len(set(urls)), 1)
        self.assertNotEqual(urls[0], self.url(KEY_B))
        self.assertEqual(len(self.records()), 2)
        with self.assertRaises(urllib.error.HTTPError) as error:
            self.request(urls[0], KEY_B)
        self.assertEqual(error.exception.code, 401)
        self.assertEqual(self.request(urls[0]).read(), b'{"hello":"world"}')
        for path in self.home.glob(".local/state/codex-rate-proxy/**/*"):
            if path.is_file():
                self.assertNotIn(KEY_A.encode(), path.read_bytes())
                self.assertNotIn(KEY_B.encode(), path.read_bytes())

    def test_key_file_env_and_stdin_share_identity(self):
        keyfile = self.home / "my-key"
        keyfile.write_text(KEY_A + "\n")
        expected = self.url()
        self.assertEqual(expected, self.cli("url", "--config", str(self.config), "--key-file", str(keyfile)).stdout.strip())
        self.assertEqual(expected, self.cli("url", "--config", str(self.config), "--key-stdin", input=KEY_A + "\n").stdout.strip())
        self.assertEqual(expected, self.cli("url", "--config", str(self.config), "--key-env", "OTHER_KEY",
            env=dict(self.env, OTHER_KEY=KEY_A)).stdout.strip())
        self.assertNotEqual(self.cli("url", "--config", str(self.config), "--key-stdin", "--ask-key", input=KEY_A, check=False).returncode, 0)

    def test_429_cooldown_is_shared_but_other_key_is_independent(self):
        a, b = self.url(), self.url(KEY_B)
        with self.assertRaises(urllib.error.HTTPError) as error:
            self.request(a, path="/limited")
        self.assertEqual(error.exception.code, 429)
        began = time.monotonic()
        self.request(b, KEY_B).read()
        self.assertLess(time.monotonic() - began, 0.7)
        self.request(a).read()
        self.assertGreaterEqual(time.monotonic() - began, 0.85)

    def test_stream_survives_prune_and_bytes_are_unchanged(self):
        url = self.url()
        response = self.request(url, path="/stream")
        first = response.readline()
        self.assertEqual(first, b'data: {"part":1}\n')
        listing = self.cli("prune").stdout
        self.assertIn("requests=1", listing)
        self.assertEqual(len(self.records()), 1)
        STREAM_RELEASE.set()
        self.assertEqual(first + response.read(), b'data: {"part":1}\n\ndata: [DONE]\n\n')
        response.close()
        wait_until(lambda: "requests=0" in self.cli("list").stdout)

    def test_launcher_injects_child_config_and_lease_survives_launcher_death(self):
        self.write_config(idle=0.3)
        report = self.home / "report"
        release = self.home / "release"
        proc = subprocess.Popen([BIN, "launch", "--config", str(self.config), "--", "--model", "fake"],
            env=dict(self.env, MOCK_REPORT=str(report), MOCK_RELEASE=str(release)),
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.processes.append(proc)
        wait_until(report.exists)
        info = json.loads(report.read_text())
        self.assertTrue(info["key_ok"])
        self.assertIn('model_provider="corp"', info["args"])
        self.assertTrue(any("model_providers.corp.base_url=" in x for x in info["args"]))
        self.assertNotIn(KEY_A, " ".join(info["args"]))
        self.assertIn("sessions=1", self.cli("list").stdout)
        self.assertNotEqual(self.cli("stop", "--config", str(self.config), check=False).returncode, 0)
        proc.kill()
        proc.wait(timeout=5)
        time.sleep(2.5)
        self.assertIn("sessions=1", self.cli("prune").stdout)
        self.assertEqual(len(self.records()), 1)
        release.touch()
        wait_until(lambda: not self.records(), timeout=8)

    def test_prune_dry_run_stop_and_dead_daemon_recovery(self):
        first = self.url()
        self.assertIn(first, self.cli("prune", "--dry-run").stdout)
        self.assertEqual(self.url(), first)
        rec = json.loads(self.records()[0].read_text())
        os.kill(rec["pid"], signal.SIGKILL)
        self.url()
        new = json.loads(self.records()[0].read_text())
        self.assertNotEqual(rec["token"], new["token"])
        self.cli("stop", "--config", str(self.config))
        wait_until(lambda: not self.records())

    def test_request_interval(self):
        self.write_config(interval=0.4)
        url = self.url()
        began = time.monotonic()
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            list(pool.map(lambda _: self.request(url).read(), range(3)))
        self.assertGreaterEqual(time.monotonic() - began, 0.75)

    def test_retry_then_success(self):
        self.write_config(retries=2)
        url = self.url()
        began = time.monotonic()
        body = b"retry-test-unique-body"
        self.assertEqual(self.request(url, path="/retry", data=body).read(), body)
        self.assertGreaterEqual(time.monotonic() - began, 0.9)

    def test_reload_and_invalid_config_keep_existing_proxy(self):
        url = self.url()
        rec_path = self.records()[0]
        rec = json.loads(rec_path.read_text())
        log_path = rec_path.parent / "proxy.log"
        self.write_config(interval=0.4)
        os.kill(rec["pid"], signal.SIGHUP)
        wait_until(lambda: "configuration reloaded:" in log_path.read_text())
        self.assertEqual(self.url(), url)
        self.request(url).read()
        began = time.monotonic()
        self.request(url).read()
        self.assertGreaterEqual(time.monotonic() - began, 0.35)
        self.config.write_text("[rate_limit]\nmin_interval_seconds=bad\n")
        os.kill(rec["pid"], signal.SIGHUP)
        wait_until(lambda: "configuration reload failed" in log_path.read_text())
        self.assertEqual(self.request(url).read(), b'{"hello":"world"}')

    def test_registration_is_explicit_private_and_reusable(self):
        env = dict(self.env)
        env.pop("OPENAI_API_KEY")
        self.cli("register", "alice", "--key-stdin", input=KEY_A + "\n", env=env)
        path = self.home / ".config/codex-rate-proxy/users/alice.key"
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(path.parent.stat().st_mode & 0o777, 0o700)
        expected = self.url()
        self.assertEqual(self.cli("url", "--config", str(self.config), "--user", "alice", env=env).stdout.strip(), expected)
        self.assertNotEqual(self.cli("register", "alice", "--key-stdin", input=KEY_B, check=False).returncode, 0)
        self.assertEqual(path.read_text().strip(), KEY_A)
        self.cli("register", "alice", "--replace", "--key-stdin", input=KEY_B)
        other = self.cli("url", "--config", str(self.config), "--user", "alice", env=env).stdout.strip()
        self.assertNotEqual(other, expected)
        for args in [("register", "../escape", "--key-stdin"),
                     ("url", "--config", str(self.config), "--user", "alice", "--key-stdin"),
                     ("url", "--config", str(self.config), "--user", "missing")]:
            self.assertNotEqual(self.cli(*args, input=KEY_A, check=False).returncode, 0)
        path.chmod(0o644)
        self.assertNotEqual(self.cli("url", "--config", str(self.config), "--user", "alice", check=False).returncode, 0)
        path.unlink()
        target = self.home / "target"
        target.write_text(KEY_A)
        path.symlink_to(target)
        self.assertNotEqual(self.cli("url", "--config", str(self.config), "--user", "alice", check=False).returncode, 0)

    def test_failed_new_launch_cleans_up_but_reused_proxy_survives(self):
        self.mock.write_text("#!/bin/sh\nexit 2\n")
        result = self.cli("launch", "--config", str(self.config), check=False)
        self.assertEqual(result.returncode, 2)
        self.assertFalse(self.records())
        existing = self.url()
        result = self.cli("launch", "--config", str(self.config), check=False)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(self.url(), existing)
        self.cli("prune")
        wait_until(lambda: not self.records())
        self.mock.unlink()
        self.assertNotEqual(self.cli("launch", "--config", str(self.config), check=False).returncode, 0)
        self.assertFalse(self.records())

    def test_shared_config_reuses_across_folders_and_daemon_has_neutral_cwd(self):
        default = self.home / ".config/codex-rate-proxy/config.ini"
        default.parent.mkdir(parents=True)
        default.write_text(self.config.read_text())
        urls = []
        for name in ("project-a", "project-b"):
            folder = self.home / name
            folder.mkdir()
            urls.append(subprocess.run([BIN, "url"], cwd=folder, env=self.env,
                capture_output=True, text=True, check=True, timeout=25).stdout.strip())
        self.assertEqual(urls[0], urls[1])
        self.assertEqual(len(self.records()), 1)
        path = self.records()[0]
        rec = json.loads(path.read_text())
        self.assertEqual(Path(os.readlink(f'/proc/{rec["pid"]}/cwd')), path.parent)

    def test_registered_launch_forwards_subcommand_and_literal_arguments(self):
        self.cli("register", "alice", "--key-stdin", input=KEY_A)
        report, release = self.home / "report", self.home / "release"
        release.touch()
        env = dict(self.env, MOCK_REPORT=str(report), MOCK_RELEASE=str(release))
        env.pop("OPENAI_API_KEY")
        args = ["exec", "--", "a prompt with spaces; $literal"]
        self.cli("launch", "--config", str(self.config), "--user", "alice", "--", *args, env=env)
        info = json.loads(report.read_text())
        self.assertEqual(info["args"][-len(args):], args)
        self.assertTrue(info["key_ok"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
