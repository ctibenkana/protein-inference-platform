"""Summarise an /v1/embeddings response read from stdin (used by make gateway-demo)."""

import json
import sys

r = json.load(sys.stdin)
if "error" in r:
    print("error:", json.dumps(r["error"]))
    sys.exit(1)
first = [round(x, 3) for x in r["data"][0]["embedding"][:3]]
print("model:", r["model"])
print("embeddings: %d, each %d numbers" % (len(r["data"]), r["dim"]))
print("usage:", r["usage"])
print("first values of the first embedding:", first)
