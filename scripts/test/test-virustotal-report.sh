#!/usr/bin/env bash
# Checks the release-body section the VirusTotal step writes, using recorded API
# responses so the formatting is pinned without spending API quota.
set -euo pipefail
cd "$(dirname "$0")/../.."
script="scripts/virustotal-report.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/clean.json" <<'JSON'
{
  "data": {
    "attributes": {
      "sha256": "8b1a9953c4611296a827abf8c47804d7c8f2ef3cb1d1b6b6c9a1b3f2e6d9a7c4",
      "last_analysis_stats": {
        "harmless": 60, "malicious": 0, "suspicious": 0,
        "undetected": 12, "timeout": 1, "type-unsupported": 1
      },
      "last_analysis_results": {}
    }
  }
}
JSON

cat > "$tmp/flagged.json" <<'JSON'
{
  "data": {
    "attributes": {
      "sha256": "deadbeef",
      "stats": {"harmless": 5, "malicious": 2, "suspicious": 1, "undetected": 4},
      "results": {
        "Alpha": {"category": "malicious", "engine_name": "Alpha", "result": "Trojan.Gen"},
        "Beta": {"category": "malicious", "engine_name": "Beta", "result": "Wacatac"},
        "Gamma": {"category": "suspicious", "engine_name": "Gamma", "result": "Suspicious"},
        "Delta": {"category": "harmless", "engine_name": "Delta", "result": null},
        "Eps": {"category": "undetected", "engine_name": "Eps", "result": null}
      }
    }
  }
}
JSON

fail=0
check() { # $1=label $2=actual $3=expected
  if [ "$2" = "$3" ]; then
    echo "ok   $1"
  else
    echo "FAIL $1"
    diff <(printf '%s\n' "$3") <(printf '%s\n' "$2") || true
    fail=1
  fi
}

export ASSET=scs-rclient-win-portable.exe
expected_clean="$(cat <<'TXT'
### VirusTotal Report

| Asset | SHA256 | Engines flagging |
| --- | --- | --- |
| `scs-rclient-win-portable.exe` | `8b1a9953c4611296a827abf8c47804d7c8f2ef3cb1d1b6b6c9a1b3f2e6d9a7c4` | 0 / 74 |

[Full report](https://www.virustotal.com/gui/file/8b1a9953c4611296a827abf8c47804d7c8f2ef3cb1d1b6b6c9a1b3f2e6d9a7c4)
TXT
)"
check "clean report renders the engine count from the stats" \
  "$("$script" render "$tmp/clean.json")" "$expected_clean"

expected_flagged="$(cat <<'TXT'
### VirusTotal Report

| Asset | SHA256 | Engines flagging |
| --- | --- | --- |
| `scs-rclient-win-portable.exe` | `deadbeef` | 3 / 5 |

Flagged by: Alpha, Beta, Gamma

[Full report](https://www.virustotal.com/gui/file/deadbeef)
TXT
)"
check "flagged report names the engines, sorted" \
  "$("$script" render "$tmp/flagged.json")" "$expected_flagged"

# A failed lookup must still leave a visible note in the release body.
check "unavailable report explains itself" \
  "$("$script" unavailable "quota exceeded (429)")" "$(cat <<'TXT'
### VirusTotal Report

VirusTotal did not return a report (quota exceeded (429)). Re-run the workflow to post it; the asset itself is unaffected.
TXT
)"

exit "$fail"
