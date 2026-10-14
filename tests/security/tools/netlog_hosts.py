"""Summarise, per canary host, which netlog event types and source types mention it.

Tolerates a truncated netlog (the app is still running): constants are on line 1,
then one event per line.
"""
import json
import re
import sys
from collections import defaultdict

path = sys.argv[1]
host_re = re.compile(r"[a-z0-9-]+\.mymdedit-canary\.invalid")
with open(path, encoding="utf-8", errors="replace") as f:
    first = f.readline()
    consts = json.loads(first.rstrip().rstrip(",").removeprefix('{"constants":') or "{}")
    ev_names = {v: k for k, v in consts["logEventTypes"].items()}
    src_names = {v: k for k, v in consts["logSourceType"].items()}
    events = []
    for line in f:
        line = line.strip().rstrip(",")
        if not line.startswith("{"):
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            pass

# Sources that ever mention a host; then every event type those sources emitted.
src_hosts = defaultdict(set)
for e in events:
    for h in host_re.findall(json.dumps(e.get("params", {}))):
        src_hosts[e["source"]["id"]].add(h)
by_host = defaultdict(lambda: defaultdict(set))
for e in events:
    sid = e["source"]["id"]
    for h in src_hosts.get(sid, ()):
        by_host[h][src_names.get(e["source"]["type"], e["source"]["type"])].add(ev_names.get(e["type"], e["type"]))
for h in sorted(by_host):
    print(h)
    for st, evs in sorted(by_host[h].items()):
        print(f"  {st}: {', '.join(sorted(evs))}")
