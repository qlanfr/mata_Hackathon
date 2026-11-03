#!/usr/bin/env python3
"""Drives `aiops mcp` the way an AI client does: initialize, list the tools, call them.

    scripts/mcp-smoke.py <path-to-aiops> [--full]

Without --full it only reads (service_info, list_runs, get_run and get_trial on the newest run).
With --full it starts an experiment through the start_experiment tool, calls wait_for_run until
the run finishes and reads the verified trial. The deployment and the admin token come from the
environment (AIOPS_URL, AIOPS_ADMIN_TOKEN); the token is never passed as a tool argument.
Exit code: 0 on success (with --full: a finished run; with ACCEPT=verified, a VERIFIED one).
"""

import json
import os
import subprocess
import sys
import time


class Client:
    def __init__(self, binary):
        self.proc = subprocess.Popen(
            [binary, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr, text=True, bufsize=1
        )
        self.next_id = 0

    def send(self, message):
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params=None):
        self.next_id += 1
        rid = self.next_id
        self.send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}})
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise SystemExit(f"server closed stdout while waiting for {method}")
            message = json.loads(line)
            if message.get("method") == "notifications/progress":
                print(f"  progress: {message['params'].get('message')}")
                continue
            if message.get("id") == rid:
                if "error" in message:
                    raise SystemExit(f"{method} failed: {message['error']}")
                return message["result"]

    def call(self, name, arguments=None, progress=None):
        params = {"name": name, "arguments": arguments or {}}
        if progress is not None:
            params["_meta"] = {"progressToken": progress}
        result = self.request("tools/call", params)
        if result.get("isError"):
            raise SystemExit(f"tool {name} failed: {result['content'][0]['text']}")
        return result["structuredContent"]

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=10)


def main():
    binary = sys.argv[1]
    full = "--full" in sys.argv
    c = Client(binary)
    init = c.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mcp-smoke", "version": "1"}})
    c.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    print(f"server {init['serverInfo']['name']} {init['serverInfo']['version']}, protocol {init['protocolVersion']}")
    tools = [t["name"] for t in c.request("tools/list")["tools"]]
    print("tools:", ", ".join(tools))

    info = c.call("service_info")
    print(f"service_info: scenario {info.get('scenario')}, provider {info.get('provider')}")
    runs = c.call("list_runs", {"limit": 3})["runs"]
    print(f"list_runs: {len(runs)} runs")

    if full:
        started = c.call("start_experiment", {})
        run_id = started["run_id"]
        print(f"start_experiment: {run_id} {started['url']}")
        deadline = time.time() + int(os.environ.get("TIMEOUT_S", "3000"))
        while True:
            run = c.call("wait_for_run", {"run_id": run_id, "timeout_s": 300}, progress=f"wait-{run_id}")
            print(f"wait_for_run: {run['state']} finished={run['finished']}")
            if run["finished"] or time.time() > deadline:
                break
    else:
        if not runs:
            print("no runs to read")
            c.close()
            return 0
        run = c.call("get_run", {"run_id": runs[0]["run_id"]})
        print(f"get_run: {run['run_id']} {run['state']}")

    for t in run["trials"]:
        print(f"  trial {t['seq']}: {t['outcome']} {t.get('reason')}")
    verified = [t for t in run["trials"] if t["outcome"] == "VERIFIED"]
    if verified:
        trial = c.call("get_trial", {"trial_id": verified[-1]["trial_id"], "max_diff_chars": 400})
        m = trial["measurements"]
        print(f"get_trial: {trial['outcome']} p95 {m['p95_ms']['baseline']:.1f} -> {m['p95_ms']['candidate']:.1f} ms, "
              f"SQL {m['sql_per_request']['baseline']} -> {m['sql_per_request']['candidate']}, golden {m['golden']}")
        print("diff starts:", trial["diff"][:200].replace("\n", " | "))
    c.close()

    summary = f"MCP {'full' if full else 'read'} run {run['run_id']}: {run['state']}, verified={run['verified']}, provider {run['provider']}"
    print("SUMMARY:", summary)
    if full and not run["finished"]:
        return 1
    if full and os.environ.get("ACCEPT", "finished") == "verified" and not run["verified"]:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
