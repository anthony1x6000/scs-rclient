#!/usr/bin/env bash
# Exercises the whole posting path of scripts/virustotal-report.sh with fake
# `gh` and `curl` binaries: a quota wall has to leave a readable note on the
# release, and a real report has to replace the previous section in place.
set -euo pipefail
cd "$(dirname "$0")/../.."
script="$PWD/scripts/virustotal-report.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/work"
export TMPDIR="$tmp"

fail=0
check() { # $1=label, $2=file, $3=substring
  if grep -qF -- "$3" "$2"; then
    echo "ok   $1"
  else
    echo "FAIL $1: missing '$3' in"; sed 's/^/     | /' "$2" | head -20
    fail=1
  fi
}
refute() { # $1=label, $2=file, $3=substring
  if grep -qF -- "$3" "$2"; then
    echo "FAIL $1: found leftover '$3'"
    fail=1
  else
    echo "ok   $1"
  fi
}

cat > "$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
  "release download")
    shift 2; dir=.; pattern=""
    while [ $# -gt 0 ]; do
      case "$1" in --dir) dir=$2; shift 2;; --pattern) pattern=$2; shift 2;; *) shift;; esac
    done
    printf 'MZ fake portable\n' > "$dir/$pattern"
    ;;
  "release view") printf '%s\n' "$FAKE_BODY" ;;
  "release edit")
    shift 2
    while [ $# -gt 0 ]; do
      case "$1" in --notes-file) cp "$2" "$CAPTURE_NOTES"; shift 2;; *) shift;; esac
    done
    ;;
  *) echo "unexpected gh invocation: $*" >&2; exit 1 ;;
esac
GH

cat > "$tmp/bin/curl" <<'CURL'
#!/usr/bin/env bash
set -euo pipefail
out=/dev/null
while [ $# -gt 0 ]; do
  case "$1" in --output) out=$2; shift 2;; *) shift;; esac
done
case "${FAKE_CURL_MODE}" in
  quota)
    printf '%s' '{"error": {"code": "QuotaExceededError", "message": "Quota exceeded"}}' > "$out"
    printf '429'
    ;;
  known)
    cat "${FAKE_CURL_BODY}" > "$out"
    printf '200'
    ;;
  *) echo "unexpected curl mode" >&2; exit 1 ;;
esac
CURL
chmod +x "$tmp/bin/gh" "$tmp/bin/curl"

run_scan() { # $1=mode $2=body $3=notes out
  FAKE_CURL_MODE=$1 FAKE_CURL_BODY=${2:-} FAKE_BODY=$'old notes\n### VirusTotal Report\nstale verdict\n' \
    CAPTURE_NOTES=$3 PATH="$tmp/bin:$PATH" \
    GH_TOKEN=fake VT_API_KEY=fake TAG=v9 ASSET=scs-rclient-win-portable.exe \
    "$script" scan >/dev/null 2>&1 || true
}

# A quota wall must post the reason, replace the stale section, and fail.
run_scan quota "" "$tmp/quota.md"
check "quota wall is explained in the release notes" "$tmp/quota.md" \
  "VirusTotal did not return a report (quota exceeded (HTTP 429))."
check "the previous notes survive" "$tmp/quota.md" "old notes"
refute "the stale section is replaced, not stacked" "$tmp/quota.md" "stale verdict"

# A real report must render and replace the stale section.
cat > "$tmp/report.json" <<'JSON'
{"data": {"attributes": {
  "sha256": "6252f9f5d222dcada0f99b4cb63c8c55ccb34107ae2517a3d0272a9359e9bddd",
  "last_analysis_stats": {"harmless": 60, "malicious": 0, "suspicious": 0, "undetected": 12, "timeout": 1, "type-unsupported": 1},
  "last_analysis_results": {}
}}}
JSON
run_scan known "$tmp/report.json" "$tmp/report.md"
check "verdict is posted" "$tmp/report.md" '| `scs-rclient-win-portable.exe` | `6252f9f5d222dcada0f99b4cb63c8c55ccb34107ae2517a3d0272a9359e9bddd` | 0 / 74 |'
check "report link is posted" "$tmp/report.md" '[Full report](https://www.virustotal.com/gui/file/6252f9f5d222dcada0f99b4cb63c8c55ccb34107ae2517a3d0272a9359e9bddd)'
refute "the stale section is gone" "$tmp/report.md" "stale verdict"

exit "$fail"
