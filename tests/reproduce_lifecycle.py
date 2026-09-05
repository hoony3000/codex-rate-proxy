"""Observe v0.3.0 lifecycle with a real Codex config error; no LLM calls."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

binary, codex = map(lambda p: str(Path(p).resolve()), sys.argv[1:3])
with tempfile.TemporaryDirectory(prefix="crp-repro-", dir="/tmp") as tmp:
    home = Path(tmp)
    env = dict(os.environ, HOME=tmp, CODEX_HOME=str(home / ".codex"),
               OPENAI_API_KEY="fake-reproduction-key")
    (home / ".codex").mkdir()
    (home / ".codex/config.toml").write_text('[model_providers.corp]\nname = [invalid TOML\n')
    config = home / ".config/codex-rate-proxy/config.ini"
    config.parent.mkdir(parents=True)
    config.write_text(f"[upstream]\nbase_url=http://127.0.0.1:1/v1\n"
                      f"[launcher]\ncodex_binary={codex}\nprovider=corp\n"
                      "[lifecycle]\nidle_timeout_seconds=1800\n")

    def run(*args, cwd=None):
        return subprocess.run([binary, *args], env=env, cwd=cwd,
                              capture_output=True, text=True, timeout=30)

    def snapshot():
        result = []
        for path in home.glob(".local/state/codex-rate-proxy/*/record.json"):
            record = json.loads(path.read_text())
            pid = record["pid"]
            result.append({"pid": pid, "state": Path(f"/proc/{pid}/stat").read_text().split()[2],
                           "cwd": os.readlink(f"/proc/{pid}/cwd")})
        return result

    try:
        for name in ("a", "b"):
            folder = home / name
            folder.mkdir()
            result = run("launch", "--", "exec", "--skip-git-repo-check", "offline reproduction", cwd=folder)
            assert result.returncode != 0, result.stdout
            assert "config" in result.stderr.lower(), result.stderr
            print(f"COMMON CONFIG folder={name} exit={result.returncode}", flush=True)
            print(result.stderr, flush=True)
            current = snapshot()
            print(json.dumps(current), flush=True)
            assert len(current) == 1, current
            assert current[0]["state"] != "Z", current
        for name in ("a", "b"):
            folder = home / name
            (folder / "proxy.ini").write_text(config.read_text())
            result = run("launch", "--config", "proxy.ini", "--", "exec",
                         "--skip-git-repo-check", "offline reproduction", cwd=folder)
            assert result.returncode != 0
            print(f"COPIED CONFIG folder={name} exit={result.returncode}", flush=True)
            print(json.dumps(snapshot()), flush=True)
        assert len(snapshot()) == 3
        print("REPRODUCED: failed Codex leaves live idle daemon; shared config reuses across cwd; copied config paths create distinct daemons.", flush=True)
    finally:
        print(run("prune").stdout, flush=True)
        time.sleep(1)
