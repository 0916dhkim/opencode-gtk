#!/usr/bin/env python3
"""Query the fake server's JSON-lines request log from shell scripts.

  logwait.py seq   LOG                                  print the last seq (0 if empty)
  logwait.py wait  LOG --expr EXPR [--after N] [--timeout S]
                   print the first matching record after seq N; exit 1 on timeout
  logwait.py none  LOG --expr EXPR [--after N]          exit 1 (printing matches) if any record matches
  logwait.py count LOG --expr EXPR [--after N]          print the number of matches

EXPR is a Python expression evaluated per record with these names bound:
  r     the record            http  r["kind"] == "http"
  q     r["query"] or {}      b     r["body"] or {}       p  r["params"] or {}
  route r.get("route")        ev    r.get("type") for kind == "event" records
"""

import argparse
import json
import sys
import time


def records(path):
    try:
        with open(path, encoding="utf-8") as stream:
            for line in stream:
                line = line.strip()
                if line:
                    try:
                        yield json.loads(line)
                    except ValueError:
                        continue
    except FileNotFoundError:
        return


def matches(record, code):
    scope = {
        "r": record,
        "http": record.get("kind") == "http",
        "q": record.get("query") or {},
        "b": record.get("body") or {},
        "p": record.get("params") or {},
        "route": record.get("route"),
        "ev": record.get("type") if record.get("kind") == "event" else None,
    }
    try:
        # Names go in globals: generator expressions in EXPR (e.g. `any(k in b ...)`)
        # get their own scope and cannot see a separate locals dict.
        names = {"__builtins__": {"any": any, "all": all, "len": len, "str": str, "isinstance": isinstance, "dict": dict, "list": list}}
        names.update(scope)
        return bool(eval(code, names))
    except Exception:  # noqa: BLE001 - a record missing a field just doesn't match
        return False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=["seq", "wait", "none", "count"])
    parser.add_argument("log")
    parser.add_argument("--expr", default="True")
    parser.add_argument("--after", type=int, default=0)
    parser.add_argument("--timeout", type=float, default=10.0)
    args = parser.parse_args()
    if args.command == "seq":
        print(max((record.get("seq", 0) for record in records(args.log)), default=0))
        return
    code = compile(args.expr, "<expr>", "eval")
    if args.command == "wait":
        deadline = time.monotonic() + args.timeout
        while True:
            for record in records(args.log):
                if record.get("seq", 0) > args.after and matches(record, code):
                    print(json.dumps(record))
                    return
            if time.monotonic() >= deadline:
                sys.exit(1)
            time.sleep(0.2)
    found = [record for record in records(args.log) if record.get("seq", 0) > args.after and matches(record, code)]
    if args.command == "count":
        print(len(found))
        return
    if found:
        for record in found[:10]:
            print(json.dumps(record))
        sys.exit(1)


if __name__ == "__main__":
    main()
