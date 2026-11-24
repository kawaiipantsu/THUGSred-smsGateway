#!/bin/sh
# Write a CycloneDX 1.5 software bill of materials from Cargo.lock.
# Usage: scripts/sbom.sh [output.json]
set -eu
cd "$(dirname "$0")/.."
OUT="${1:-dist/sbom.cdx.json}"
mkdir -p "$(dirname "$OUT")"
command -v python3 >/dev/null 2>&1 || { echo "error: python3 is required to generate the SBOM" >&2; exit 1; }
python3 - "$OUT" <<'PY'
import json, re, sys
packages, cur = [], None
for line in open("Cargo.lock", encoding="utf-8"):
    line = line.rstrip("\n")
    if line == "[[package]]":
        cur = {}
        packages.append(cur)
    elif cur is not None:
        m = re.match(r'^(name|version|source|checksum) = "(.*)"$', line)
        if m:
            cur[m.group(1)] = m.group(2)
root = next(p for p in packages if p["name"] == "thugsred-smsgateway")
components = []
for p in sorted(packages, key=lambda p: (p["name"], p["version"])):
    if p is root:
        continue
    c = {"type": "library", "name": p["name"], "version": p["version"],
         "purl": "pkg:cargo/%s@%s" % (p["name"], p["version"])}
    if "checksum" in p:
        c["hashes"] = [{"alg": "SHA-256", "content": p["checksum"]}]
    components.append(c)
doc = {"bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
       "metadata": {"component": {"type": "application", "name": "THUGSred-smsGateway",
                                  "version": root["version"],
                                  "purl": "pkg:cargo/thugsred-smsgateway@" + root["version"]}},
       "components": components}
json.dump(doc, open(sys.argv[1], "w", encoding="utf-8"), indent=1)
print("wrote %s (%d components)" % (sys.argv[1], len(components)))
PY
