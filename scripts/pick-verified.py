#!/usr/bin/env python3
"""Prints `<run_id> <trial_id>` of a VERIFIED run on the deployment, or nothing.

    pick-verified.py <aiops binary> <run_id>     that run (fails if it has no VERIFIED trial)
    pick-verified.py <aiops binary> latest       the newest run with a VERIFIED trial
    pick-verified.py <aiops binary> scheduled    the newest scheduled run of the last 26 hours, if
                                                 it reached VERIFIED (26 hours: GitHub can start the
                                                 nightly workflow hours late, past UTC midnight)

Uses the read-only `aiops` commands, so no token is needed.
"""
import datetime
import json
import subprocess
import sys


def aiops(binary, *args):
    out = subprocess.run([binary, "--json", *args], check=True, capture_output=True, text=True)
    return json.loads(out.stdout)


def verified_trial(binary, run_id):
    status = aiops(binary, "status", run_id)
    trials = [t for t in status.get("trials", []) if t.get("outcome") == "VERIFIED"]
    return trials[-1]["trial_id"] if trials else None


def main():
    binary, which = sys.argv[1], sys.argv[2]
    if which not in ("latest", "scheduled"):
        trial = verified_trial(binary, which)
        if not trial:
            sys.exit(f"run {which} has no VERIFIED trial")
        print(which, trial)
        return
    since = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(hours=26)
    start_ms = int(since.timestamp() * 1000)
    for run in aiops(binary, "runs", "--limit", "50"):
        if which == "scheduled":
            if (run.get("trigger") or {}).get("kind") != "schedule" or run.get("created_at", 0) < start_ms:
                continue
            # The newest scheduled run decides, whatever its state; say what it was.
            print(f"newest scheduled run: {run['id']} {run.get('state')}", file=sys.stderr)
            trial = verified_trial(binary, run["id"]) if run.get("state") == "COMPLETED" else None
            if trial:
                print(run["id"], trial)
            return
        if run.get("state") != "COMPLETED":
            continue
        trial = verified_trial(binary, run["id"])
        if trial:
            print(run["id"], trial)
            return
    if which == "scheduled":
        print("no scheduled run in the last 26 hours", file=sys.stderr)


if __name__ == "__main__":
    main()
