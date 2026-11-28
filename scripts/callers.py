#!/usr/bin/env python3
"""Manage the embeddings API's callers (API keys) on the cluster.

The gateway reads the Secret inference/gateway-callers, which holds only the
SHA-256 of each key. Keys are generated here and shown once (or stored
straight into another Secret); they are never written to git or logs.

  callers.py add ID [--residues-per-minute N] [--max-inflight-tokens N]
      Create a key for a new caller and print it once, to hand over.
  callers.py revoke ID
  callers.py limits ID [--residues-per-minute N] [--max-inflight-tokens N]
      Change a caller's limits; its key stays the same. 0 resets to default.
  callers.py list
  callers.py ensure ID --store-key-in SECRET [--synthetic] [limits]
      Make sure ID exists with its key stored in SECRET (key "api-key");
      prints nothing secret. Used for the load-test and demo callers.

The gateway picks up changes within about a minute (Secret propagation plus
its 30-second reload). IDs are pseudonyms: they appear in metrics and logs,
so use "lab-01", not a person's name.
"""

import argparse
import base64
import hashlib
import json
import re
import secrets
import subprocess
import sys

NAMESPACE = "inference"
SECRET = "gateway-callers"
FILE_KEY = "callers.json"
ID_RE = re.compile(r"^[a-z0-9._-]{1,64}$")


def kubectl(*args, input_text=None, check=True):
    return subprocess.run(
        ["kubectl", "-n", NAMESPACE, *args],
        input=input_text, capture_output=True, text=True, check=check,
    )


def read_secret(name):
    r = kubectl("get", "secret", name, "-o", "json", check=False)
    if r.returncode != 0:
        if "NotFound" in r.stderr:
            return None
        sys.exit("kubectl failed: " + r.stderr.strip())
    return {k: base64.b64decode(v).decode() for k, v in json.loads(r.stdout).get("data", {}).items()}


def write_secret(name, key, value):
    manifest = kubectl(
        "create", "secret", "generic", name, "--from-file=%s=/dev/stdin" % key,
        "--dry-run=client", "-o", "yaml", input_text=value,
    ).stdout
    kubectl("apply", "-f", "-", input_text=manifest)


def load_callers():
    data = read_secret(SECRET)
    if not data or FILE_KEY not in data:
        return []
    return json.loads(data[FILE_KEY])["callers"]


def save_callers(callers):
    write_secret(SECRET, FILE_KEY, json.dumps({"callers": callers}, indent=2) + "\n")


def new_key():
    return "pip_" + secrets.token_urlsafe(32)


def entry(caller_id, key, args):
    e = {"id": caller_id, "key_sha256": hashlib.sha256(key.encode()).hexdigest()}
    if args.residues_per_minute:
        e["residues_per_minute"] = args.residues_per_minute
    if args.max_inflight_tokens:
        e["max_inflight_tokens"] = args.max_inflight_tokens
    if getattr(args, "synthetic", False):
        e["synthetic"] = True
    return e


def check_id(caller_id):
    if not ID_RE.match(caller_id):
        sys.exit("ID must be 1-64 lowercase letters, digits, '.', '_' or '-' (a pseudonym, not a name)")


def cmd_add(args):
    check_id(args.id)
    callers = load_callers()
    if any(c["id"] == args.id for c in callers):
        sys.exit("caller %s already exists; revoke it first to issue a new key" % args.id)
    key = new_key()
    callers.append(entry(args.id, key, args))
    save_callers(callers)
    print("Created caller %s. Its API key, shown only this once:\n" % args.id)
    print("    " + key + "\n")
    print("Give it to the user over a private channel. They send it as")
    print("    Authorization: Bearer <key>")
    print("It works within about a minute. Lost keys can't be recovered: revoke and add again.")


def cmd_revoke(args):
    callers = load_callers()
    kept = [c for c in callers if c["id"] != args.id]
    if len(kept) == len(callers):
        sys.exit("no caller %s" % args.id)
    save_callers(kept)
    print("Revoked %s; requests with its key are refused within about a minute." % args.id)


def cmd_limits(args):
    callers = load_callers()
    c = next((c for c in callers if c["id"] == args.id), None)
    if c is None:
        sys.exit("no caller %s" % args.id)
    for field, value in (("residues_per_minute", args.residues_per_minute), ("max_inflight_tokens", args.max_inflight_tokens)):
        if value == 0:
            c.pop(field, None)
        elif value:
            c[field] = value
    save_callers(callers)
    print("Updated %s; the gateway applies it within about a minute." % args.id)


def cmd_list(_args):
    callers = load_callers()
    if not callers:
        print("no callers")
    for c in callers:
        limits = ", ".join("%s=%s" % (k, c[k]) for k in ("residues_per_minute", "max_inflight_tokens") if k in c)
        print("%-20s %s%s" % (c["id"], limits or "default limits", " (synthetic)" if c.get("synthetic") else ""))


def cmd_ensure(args):
    check_id(args.id)
    callers = load_callers()
    stored = read_secret(args.store_key_in)
    current = next((c for c in callers if c["id"] == args.id), None)
    if current and stored and "api-key" in stored:
        wanted = entry(args.id, stored["api-key"], args)
        if current == wanted:
            return  # already in place
    key = new_key()
    callers = [c for c in callers if c["id"] != args.id] + [entry(args.id, key, args)]
    write_secret(args.store_key_in, "api-key", key)
    save_callers(callers)
    print("caller %s ready (key stored in Secret %s/%s)" % (args.id, NAMESPACE, args.store_key_in))


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="command", required=True)
    for name in ("add", "ensure"):
        s = sub.add_parser(name)
        s.add_argument("id")
        s.add_argument("--residues-per-minute", type=int)
        s.add_argument("--max-inflight-tokens", type=int)
        if name == "ensure":
            s.add_argument("--store-key-in", required=True)
            s.add_argument("--synthetic", action="store_true")
    sub.add_parser("revoke").add_argument("id")
    lim = sub.add_parser("limits")
    lim.add_argument("id")
    lim.add_argument("--residues-per-minute", type=int)
    lim.add_argument("--max-inflight-tokens", type=int)
    sub.add_parser("list")
    args = p.parse_args()
    {"add": cmd_add, "revoke": cmd_revoke, "limits": cmd_limits, "list": cmd_list, "ensure": cmd_ensure}[args.command](args)


if __name__ == "__main__":
    main()
