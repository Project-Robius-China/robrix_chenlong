#!/usr/bin/env python3
"""Check Matrix homeserver health and capabilities."""

import json
import sys

try:
    import requests
except ImportError:
    print("Install requests: pip install requests")
    sys.exit(1)

HOMESERVER = "http://172.16.203.27:8128"
TIMEOUT = 10
SESSION = requests.Session()
SESSION.headers.update({"User-Agent": "matrix-check/1.0", "Accept": "application/json"})


def check(label, url):
    print(f"\n[{label}]")
    print(f"  URL: {url}")
    try:
        resp = SESSION.get(url, timeout=TIMEOUT)
        print(f"  Status: {resp.status_code}")
        try:
            data = resp.json()
            print(f"  Response: {json.dumps(data, indent=4)}")
            return resp.ok, data
        except json.JSONDecodeError:
            print(f"  Response: {resp.text[:200]}")
            return resp.ok, None
    except requests.exceptions.ConnectionError as e:
        print(f"  Error: connection refused — {e}")
        return False, None
    except requests.exceptions.Timeout:
        print(f"  Error: timed out after {TIMEOUT}s")
        return False, None
    except Exception as e:
        print(f"  Error: {e}")
        return False, None


def main():
    print(f"Matrix Homeserver Check: {HOMESERVER}")
    print("=" * 60)

    results = {}

    ok, data = check("Well-known client", f"{HOMESERVER}/.well-known/matrix/client")
    results["well_known"] = ok
    if ok and data:
        base = data.get("m.homeserver", {}).get("base_url")
        if base:
            print(f"  Advertised base_url: {base}")

    ok, data = check("Client versions", f"{HOMESERVER}/_matrix/client/versions")
    results["versions"] = ok
    if ok and data:
        versions = data.get("versions", [])
        print(f"  Supported versions: {', '.join(versions)}")

    ok, data = check("Login flows", f"{HOMESERVER}/_matrix/client/v3/login")
    results["login"] = ok
    if ok and data:
        flows = [f.get("type") for f in data.get("flows", [])]
        print(f"  Login flows: {', '.join(flows)}")

    ok, data = check("Federation server version", f"{HOMESERVER}/_matrix/federation/v1/version")
    results["federation"] = ok
    if ok and data:
        server = data.get("server", {})
        print(f"  Server: {server.get('name')} {server.get('version')}")

    print("\n" + "=" * 60)
    print("Summary:")
    all_ok = True
    for name, status in results.items():
        icon = "PASS" if status else "FAIL"
        print(f"  [{icon}] {name}")
        if not status:
            all_ok = False

    if all_ok:
        print("\nHomeserver appears healthy.")
    else:
        print("\nSome checks failed — homeserver may be partially unavailable.")
        sys.exit(1)


if __name__ == "__main__":
    main()
