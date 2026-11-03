#!/usr/bin/env python3
"""Prints `<run_id> <trial_id>` of a VERIFIED run on the deployment, or nothing.

    pick-verified.py <aiops binary> <run_id>     that run (fails if it has no VERIFIED trial)
    pick-verified.py <aiops binary> latest       the newest run with a VERIFIED trial
    pick-verified.py <aiops binary> scheduled    the newest scheduled run started today (UTC),
                                                 if it reached VERIFIED

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
    today = datetime.datetime.now(datetime.timezone.utc).replace(hour=0, minute=0, second=0, microsecond=0)
    start_ms = int(today.timestamp() * 1000)
    for run in aiops(binary, "runs", "--limit", "50"):
        if run.get("state") != "COMPLETED":
            continue
        if which == "scheduled":
            if (run.get("trigger") or {}).get("kind") != "schedule" or run.get("created_at", 0) < start_ms:
                continue
        trial = verified_trial(binary, run["id"])
        if trial:
            print(run["id"], trial)
            return
        if which == "scheduled":
            return


if __name__ == "__main__":
    main()
