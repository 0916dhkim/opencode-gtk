#!/usr/bin/env python3
"""Verify a file against an npm `dist.integrity` value (sha512-<base64>)."""
import base64
import hashlib
import sys


def main() -> int:
    path, integrity = sys.argv[1], sys.argv[2]
    algo, _, expected = integrity.partition("-")
    if algo != "sha512" or not expected:
        print(f"unsupported integrity value: {integrity}", file=sys.stderr)
        return 2
    with open(path, "rb") as handle:
        actual = base64.b64encode(hashlib.sha512(handle.read()).digest()).decode()
    if actual != expected:
        print(f"integrity mismatch for {path}", file=sys.stderr)
        return 1
    print(f"verified {path} sha512-{actual}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
