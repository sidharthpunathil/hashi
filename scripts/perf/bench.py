#!/usr/bin/env python3
"""Startup and memory bench for a Hashi release binary, against fake agents.

usage: bench.py BINARY startup|load|held|big|leak [N]

  startup  time to the first /healthz answer, first start and restart (N runs)
  load     RSS idle and after N (1000) completed requests
  leak     RSS after 1000 requests, then every N/5 of N (10000) more
  held     memory per held long poll (100) and per open SSE stream (50)
  big      peak memory for one 9 MB answer

Each server gets a fresh temp dir for HASHI_HOME, HOME and the fake agent
scripts. Usage polling and the update check are off, so nothing leaves the
host and no real agent CLI runs. See docs/performance.md.

Environment: BENCH_PREEXEC=1 runs each fake agent once before the server
starts (macOS scans a new executable on its first run); BENCH_VDELAY=0.1
makes each `--version` take that many seconds; BENCH_PROMPT_BYTES pads the
prompts of the held scenario.
"""
import http.client
import json
import os
import re
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time

FAKE = r"""#!/bin/sh
if [ "$1" = "--version" ]; then [ -n "$BENCH_VDELAY" ] && sleep "$BENCH_VDELAY"; echo "9.9.9 (Fake)"; exit 0; fi
for last; do :; done
case "$last" in
  slow*) sleep 300 ;;
  big*) head -c 9000000 /dev/zero | tr '\0' 'a' > "$PWD/.big"; printf '{"type":"result","is_error":false,"result":"'; cat "$PWD/.big"; printf '","session_id":"s","total_cost_usd":0,"usage":{"input_tokens":1,"output_tokens":1}}\n'; rm -f "$PWD/.big"; exit 0 ;;
esac
printf '{"type":"result","is_error":false,"result":"ok %s","session_id":"s","total_cost_usd":0.01,"usage":{"input_tokens":5,"output_tokens":7}}\n' "$last"
"""

SLOW = "slow" + "x" * int(os.environ.get("BENCH_PROMPT_BYTES", "0"))
AGENTS = ["claude", "codex", "gemini", "opencode", "qwen", "cursor"]


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def rss_kib(pid):
    out = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout
    return int(out.strip() or 0)


def peak_kib(pid):
    """Kernel high-water mark: VmHWM on Linux, peak physical footprint on macOS."""
    if sys.platform == "linux":
        for line in open(f"/proc/{pid}/status"):
            if line.startswith("VmHWM:"):
                return int(line.split()[1])
        return 0
    out = subprocess.run(["heap", "-s", str(pid)], capture_output=True, text=True).stdout
    m = re.search(r"Physical footprint \(peak\):\s+([\d.]+)([KMG])", out)
    mult = {"K": 1, "M": 1024, "G": 1024 * 1024}[m.group(2)]
    return int(float(m.group(1)) * mult)


def live_kib(pid):
    """Live malloc bytes on macOS (heap); RSS elsewhere."""
    if sys.platform == "linux":
        return rss_kib(pid)
    out = subprocess.run(["heap", "-s", str(pid)], capture_output=True, text=True).stdout
    m = re.search(r"All zones: \d+ nodes \((\d+) bytes\)", out)
    return int(m.group(1)) // 1024


class Server:
    def __init__(self, binary, root=None, extra=(), conc="8"):
        self.root = root or tempfile.mkdtemp(prefix="hashi-bench-")
        bindir = os.path.join(self.root, "bin")
        os.makedirs(bindir, exist_ok=True)
        os.makedirs(os.path.join(self.root, "user"), exist_ok=True)
        env = {
            "PATH": bindir + ":/usr/bin:/bin",
            "HOME": os.path.join(self.root, "user"),
            "HASHI_HOME": os.path.join(self.root, "home"),
            "HASHI_USAGE_POLL": "0",
            "HASHI_NO_UPDATE_CHECK": "1",
            "TMPDIR": self.root,
        }
        if os.environ.get("BENCH_VDELAY"):
            env["BENCH_VDELAY"] = os.environ["BENCH_VDELAY"]
        for a in AGENTS:
            path = os.path.join(bindir, a)
            with open(path, "w") as f:
                f.write(FAKE)
            os.chmod(path, 0o755)
            env[f"HASHI_{a.upper()}_BIN"] = path
            if os.environ.get("BENCH_PREEXEC"):
                subprocess.run([path, "--version"], capture_output=True)
        self.port = free_port()
        args = [binary, "serve", "--headless", "--no-update-check", "--no-usage-poll",
                "--port", str(self.port), "--rate-limit", "0", "--concurrency", conc,
                "--max-running", conc, "--memory-reserve", "0", "--disk-floor", "0",
                "--nice", "0", "--queue-limit", "1000", *extra]
        self.t0 = time.perf_counter()
        self.proc = subprocess.Popen(args, env=env, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, text=True)
        self.ready = self.wait_healthz() - self.t0
        self.admin = None
        lines = []
        while root is None:
            line = self.proc.stdout.readline()
            lines.append(line)
            if "admin token" in line:
                self.admin = self.proc.stdout.readline().strip()
                break
            if not line or len(lines) > 50:
                break
        threading.Thread(target=self._drain, daemon=True).start()

    def _drain(self):
        for _ in self.proc.stdout:
            pass

    def wait_healthz(self):
        while True:
            try:
                c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=1)
                c.request("GET", "/healthz")
                if c.getresponse().status == 200:
                    return time.perf_counter()
            except OSError:
                pass
            if self.proc.poll() is not None:
                raise SystemExit("server exited")
            time.sleep(0.0005)

    def conn(self):
        return http.client.HTTPConnection("127.0.0.1", self.port, timeout=600)

    def call(self, c, method, path, body=None, token=None):
        h = {"Authorization": f"Bearer {token or self.admin}", "Content-Type": "application/json"}
        c.request(method, path, body=json.dumps(body) if body is not None else None, headers=h)
        r = c.getresponse()
        data = r.read()
        return r.status, json.loads(data) if data else None

    def token(self, name):
        st, body = self.call(self.conn(), "POST", "/v1/tokens", {"name": name, "role": "admin"})
        assert st == 201, (st, body)
        return body["token"]

    def rss(self):
        return rss_kib(self.proc.pid)

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(10)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        shutil.rmtree(self.root, ignore_errors=True)


def complete(s, n, workers=8):
    """Sends n prompts with wait=120 from `workers` threads (one token each)."""
    tokens = [s.token(f"w{i}") for i in range(workers)]
    left = [n]
    lock = threading.Lock()
    bad = [0]

    def work(tok):
        c = s.conn()
        while True:
            with lock:
                if left[0] <= 0:
                    return
                left[0] -= 1
                k = left[0]
            st, body = s.call(c, "POST", "/v1/ask", {"message": f"hi {k}", "wait": 120}, tok)
            if st != 200 or body.get("status") != "completed":
                bad[0] += 1

    ts = [threading.Thread(target=work, args=(t,)) for t in tokens]
    t0 = time.perf_counter()
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    return time.perf_counter() - t0, bad[0]


def raw_open(s, method, path, body, token):
    sock = socket.create_connection(("127.0.0.1", s.port))
    data = json.dumps(body).encode() if body is not None else b""
    req = (f"{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{s.port}\r\n"
           f"Authorization: Bearer {token}\r\nContent-Type: application/json\r\n"
           f"Content-Length: {len(data)}\r\n\r\n").encode() + data
    sock.sendall(req)
    return sock


def main():
    binary, mode = sys.argv[1], sys.argv[2]
    n = int(sys.argv[3]) if len(sys.argv) > 3 else 0
    if mode == "startup":
        cold, warm = [], []
        for _ in range(n or 15):
            s = Server(binary)
            cold.append(s.ready)
            root = s.root
            s.proc.terminate()
            s.proc.wait()
            s2 = Server(binary, root=root)
            warm.append(s2.ready)
            s2.stop()
        print(json.dumps({"cold_ms": round(statistics.median(cold) * 1000, 1),
                          "warm_ms": round(statistics.median(warm) * 1000, 1)}))
    elif mode == "load":
        s = Server(binary)
        time.sleep(1)
        idle = s.rss()
        secs, bad = complete(s, n or 1000)
        time.sleep(1)
        print(json.dumps({"idle_kib": idle, "after_kib": s.rss(), "requests": n or 1000,
                          "secs": round(secs, 1), "bad": bad}))
        s.stop()
    elif mode == "leak":
        s = Server(binary)
        complete(s, 1000)
        base = s.rss()
        marks = []
        for _ in range(5):
            complete(s, (n or 10000) // 5)
            marks.append(s.rss())
        print(json.dumps({"after_1k_kib": base, "marks_kib": marks,
                          "growth_kib": marks[-1] - base}))
        s.stop()
    elif mode == "held":
        s = Server(binary, conc="1")
        time.sleep(1)
        idle = s.rss()
        # Tokens and queued requests first, so the deltas below are the
        # held connections alone.
        polls_for = []
        for i in range(25):
            tok = s.token(f"p{i}")
            st, body = s.call(s.conn(), "POST", "/v1/ask", {"message": SLOW, "wait": 0}, tok)
            assert st == 202, (st, body)
            polls_for.append((tok, body["id"]))
        stream_toks = [s.token(f"s{i}") for i in range(50)]
        time.sleep(2)
        base = s.rss()
        base_live = live_kib(s.proc.pid)
        socks = []
        # 25 tokens x 4 long polls on requests that do not finish.
        for tok, rid in polls_for:
            for _ in range(4):
                socks.append(raw_open(s, "GET", f"/v1/ask/{rid}?wait=120", None, tok))
        time.sleep(2)
        polls = s.rss()
        polls_live = live_kib(s.proc.pid)
        # 50 SSE streams, one per token (each takes a wait slot).
        for tok in stream_toks:
            sk = raw_open(s, "POST", "/v1/chat/completions",
                          {"model": "claude", "stream": True,
                           "messages": [{"role": "user", "content": SLOW}]}, tok)
            sk.settimeout(5)
            assert b"200" in sk.recv(4096)[:20]
            socks.append(sk)
        time.sleep(2)
        streams = s.rss()
        streams_live = live_kib(s.proc.pid)
        print(json.dumps({"live_per_poll_kib": round((polls_live - base_live) / 100, 1),
                          "live_per_stream_kib": round((streams_live - polls_live) / 50, 1),
                          "idle_kib": idle, "base_kib": base, "polls100_kib": polls,
                          "plus_sse50_kib": streams,
                          "per_poll_kib": round((polls - base) / 100, 1),
                          "per_stream_kib": round((streams - polls) / 50, 1)}))
        for sk in socks:
            sk.close()
        s.stop()
    elif mode == "big":
        s = Server(binary)
        time.sleep(1)
        idle = s.rss()
        peak = [idle]
        stop = [False]

        def sample():
            while not stop[0]:
                peak[0] = max(peak[0], s.rss())
                time.sleep(0.005)

        th = threading.Thread(target=sample)
        th.start()
        st, body = s.call(s.conn(), "POST", "/v1/ask", {"message": "big", "wait": 120})
        stop[0] = True
        th.join()
        ok = st == 200 and body.get("status") == "completed" and len(body.get("response") or "") >= 9000000
        time.sleep(1)
        print(json.dumps({"idle_kib": idle, "hwm_kib": peak_kib(s.proc.pid), "peak_kib": peak[0],
                          "after_kib": s.rss(), "ok": ok,
                          "status": st, "state": body.get("status")}))
        s.stop()


if __name__ == "__main__":
    main()
