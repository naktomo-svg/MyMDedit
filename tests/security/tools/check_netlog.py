"""Pass/fail over a WebView2 netlog for the canary hosts (*.mymdedit-canary.invalid).

    py -I check_netlog.py <netlog> [--allow HOST]... [--require HOST]...

A canary host fails the check when the netlog shows a URL_REQUEST for it: a request
the preview tried to send. Name resolution and connects without a request (clicked
links, F8's accepted risk) do not count. --allow exempts a host (the accepted https
image); --require makes its absence a failure, so a broken observation never passes.
Exit code 0 = pass, 1 = fail.
"""
import argparse
import json
import re
import sys
from collections import defaultdict

ap = argparse.ArgumentParser()
ap.add_argument("netlog")
ap.add_argument("--allow", action="append", default=[])
ap.add_argument("--require", action="append", default=[])
args = ap.parse_args()

host_re = re.compile(r"[a-z0-9-]+\.mymdedit-canary\.invalid")
with open(args.netlog, encoding="utf-8", errors="replace") as f:
    first = f.readline()
    consts = json.loads(first.rstrip().rstrip(",").removeprefix('{"constants":') or "{}")
    src_names = {v: k for k, v in consts["logSourceType"].items()}
    events = []
    for line in f:
        line = line.strip().rstrip(",")
        if line.startswith("{"):
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                pass

kinds = defaultdict(set)  # host -> source types that mention it
for e in events:
    for h in host_re.findall(json.dumps(e.get("params", {}))):
        kinds[h.split(".")[0]].add(src_names.get(e["source"]["type"], str(e["source"]["type"])))

failed = False
for host, srcs in sorted(kinds.items()):
    requested = "URL_REQUEST" in srcs
    verdict = "allowed" if host in args.allow else ("FAIL: request" if requested else "ok (no request)")
    failed |= requested and host not in args.allow
    print(f"  netlog {host}: {verdict} [{', '.join(sorted(srcs))}]")
for host in args.require:
    if "URL_REQUEST" not in kinds.get(host, ()):
        print(f"  netlog {host}: FAIL: positive control missing (observation broken)")
        failed = True
sys.exit(1 if failed else 0)
