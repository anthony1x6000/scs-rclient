#!/usr/bin/env bash
# Appends a VirusTotal verdict for one release asset to the release notes.
#
# The free VirusTotal API allows a few requests per minute, so the report is
# looked up by SHA-256 first (one request when the sample is already known) and
# only uploaded when it is unknown. The section replaces any previous one, so a
# retry after a rate limit does not stack reports.
set -euo pipefail

marker="### VirusTotal Report"
report_base="https://www.virustotal.com/gui/file"
api_base="https://www.virustotal.com/api/v3"

die() { echo "error: $*" >&2; exit 1; }

usage() {
  cat <<'USAGE'
usage: virustotal-report.sh scan
       virustotal-report.sh render <virustotal-json>
       virustotal-report.sh unavailable <reason>

  scan         download $ASSET from release $TAG, ask VirusTotal for the
               verdict, and append it to the release notes. Needs VT_API_KEY,
               GH_TOKEN, TAG and ASSET.
  render       print the release-body section for an already fetched VirusTotal
               JSON file (a /files or /analyses response); no network access.
  unavailable  print the section that records a failed lookup, so a rate limit
               is visible on the release instead of silently missing.
USAGE
}

render() { # $1=json file
  jq -r --arg asset "${ASSET:-asset}" --arg base "$report_base" --arg marker "$marker" '
    def attrs: .data.attributes;
    def results: (attrs | .last_analysis_results // .results // {});
    def stats: (attrs | .last_analysis_stats // .stats // {});
    (attrs | .sha256 // "unknown") as $sha
    | (results) as $r
    | (stats) as $s
    | ([$r[] | select(.category == "malicious" or .category == "suspicious") | .engine_name]
       | unique | sort) as $flagged
    | (if ($r | length) > 0 then ($r | length) else ([$s[]] | add // 0) end) as $engines
    | "\($marker)\n\n| Asset | SHA256 | Engines flagging |\n| --- | --- | --- |\n"
      + "| `\($asset)` | `\($sha)` | \($flagged | length) / \($engines) |\n"
      + (if ($flagged | length) > 0 then "\nFlagged by: \($flagged | join(", "))\n" else "" end)
      + "\n[Full report](\($base)/\($sha))"
  ' "$1"
}

unavailable() { # $1=reason
  printf '%s\n\nVirusTotal did not return a report (%s). Re-run the workflow to post it; the asset itself is unaffected.\n' \
    "$marker" "$1"
}

# Requests are bounded and their status is inspected explicitly: the previous
# version retried a stalled connection three times with a five-minute budget
# each, which is how a stuck scan turned into a fifteen-minute silent job. The
# body lands in $tmp/body either way, so a failure can quote what VirusTotal
# actually said.
vt_fetch() { # $1=method $2=path [$3...] → prints the HTTP status code
  local method=$1 path=$2 code
  shift 2
  code=$(curl --silent --show-error --http1.1 \
      --connect-timeout 10 --max-time "${VT_TIMEOUT:-45}" \
      --output "$tmp/body" --write-out '%{http_code}' \
      --request "$method" \
      --header "x-apikey: ${VT_API_KEY:?VT_API_KEY is not set}" \
      "$@" "${api_base}${path}" 2>"$tmp/curl.err") || code="000"
  printf '%s' "$code"
}

vt_body() { head -c 2000 "$tmp/body" 2>/dev/null || true; }

# 404 simply means the sample was never seen before, which is the one answer
# worth continuing past. Every other code has to stop the run, or a rate limit
# would be mistaken for an unknown sample and turn into a second doomed request.
lookup() { # $1=sha256 → report JSON on stdout, empty when unknown, 3 on failure
  local sha=$1 code
  code=$(vt_fetch GET "/files/$sha")
  case "$code" in
    200) cat "$tmp/body" ;;
    404) : ;;
    *)
      # A quota wall is the common failure and deserves a short, readable reason
      # in the release note; the raw body still goes to the job log.
      VT_REASON="HTTP $code"
      if grep -q "QuotaExceededError" "$tmp/body" 2>/dev/null; then
        VT_REASON="quota exceeded (HTTP $code)"
      elif [ "$code" = "000" ]; then
        VT_REASON="no response from VirusTotal"
      fi
      # The caller reads this file: a command substitution runs the function in
      # a subshell, so a global would not survive the return.
      printf '%s' "$VT_REASON" > "$tmp/reason"
      echo "lookup returned HTTP $code: $(vt_body)" >&2
      [ -s "$tmp/curl.err" ] && sed 's/^/curl: /' "$tmp/curl.err" >&2
      return 3
      ;;
  esac
}

upload() { # $1=file → analysis id
  local file=$1 code
  code=$(VT_TIMEOUT=240 vt_fetch POST /files --form "file=@$file")
  if [ "$code" != "200" ]; then
    echo "upload returned HTTP $code: $(vt_body)" >&2
    return 1
  fi
  jq -r '.data.id // empty' "$tmp/body"
}

# The free tier allows about four requests a minute, so a tight poll loop trips
# the limiter in the first place: ask every 45 seconds, and stop after about
# four minutes rather than spinning.
wait_analysis() { # $1=analysis id → report JSON
  local id=$1 attempt code status
  for attempt in $(seq 1 6); do
    code=$(vt_fetch GET "/analyses/$id")
    [ "$code" = "200" ] || { echo "analysis poll returned HTTP $code" >&2; return 1; }
    status=$(jq -r '.data.attributes.status // empty' "$tmp/body")
    case "$status" in
      completed) cat "$tmp/body"; return 0 ;;
      queued|"in-progress") echo "analysis $id is $status (poll $attempt/6)" >&2; sleep 45 ;;
      *) echo "analysis $id reported status '${status:-none}'" >&2; return 1 ;;
    esac
  done
  echo "analysis $id did not finish within the poll budget" >&2
  return 1
}

append_section() { # $1=section text
  local body="$tmp/body.md" next="$tmp/body.next.md"
  gh release view "$TAG" --json body --jq '.body // ""' > "$body"
  # Everything above our marker survives; a stale report is replaced, not stacked.
  awk -v marker="$marker" 'index($0, marker) == 1 { exit } { print }' "$body" > "$next"
  { printf '%s\n\n' "$(cat "$next")"; printf '%s\n' "$1"; } > "$tmp/body.final.md"
  gh release edit "$TAG" --notes-file "$tmp/body.final.md"
}

scan() {
  : "${TAG:?TAG is not set}" "${ASSET:?ASSET is not set}" "${GH_TOKEN:?GH_TOKEN is not set}"
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT

  gh release download "$TAG" --pattern "$ASSET" --dir "$tmp" --clobber
  local file="$tmp/$ASSET" sha report reason
  [ -f "$file" ] || die "$ASSET is not attached to $TAG"
  sha=$(sha256sum "$file" | awk '{print $1}')
  echo "scanning $ASSET ($sha) on VirusTotal"

  echo "asking VirusTotal for a known sample report"
  if ! report=$(lookup "$sha" 2>"$tmp/lookup.err"); then
    reason=$(cat "$tmp/reason" 2>/dev/null || true)
    reason="${reason:-lookup failed}"
    sed 's/^/virustotal: /' "$tmp/lookup.err" >&2
    append_section "$(unavailable "$reason")"
    echo "::error::VirusTotal lookup failed: $reason"
    exit 1
  fi
  if [ -z "$report" ]; then
    echo "VirusTotal has not seen this sample; uploading it once"
    if ! id=$(upload "$file" 2>"$tmp/upload.err") || [ -z "$id" ]; then
      reason=$(tail -1 "$tmp/upload.err" 2>/dev/null)
      sed 's/^/virustotal: /' "$tmp/upload.err" >&2 2>/dev/null || true
      append_section "$(unavailable "${reason:-upload rejected}")"
      echo "::error::VirusTotal upload failed: ${reason:-upload rejected}"
      exit 1
    fi
    if ! report=$(wait_analysis "$id" 2>"$tmp/analysis.err"); then
      reason=$(tail -1 "$tmp/analysis.err" 2>/dev/null)
      sed 's/^/virustotal: /' "$tmp/analysis.err" >&2 2>/dev/null || true
      append_section "$(unavailable "${reason:-analysis did not complete}")"
      echo "::error::VirusTotal analysis failed: ${reason:-analysis did not complete}"
      exit 1
    fi
  fi

  printf '%s' "$report" > "$tmp/report.json"
  local section
  section=$(render "$tmp/report.json")
  append_section "$section"
  echo "$section"
}

case "${1:-scan}" in
  render) render "${2:?path to a VirusTotal JSON file}" ;;
  unavailable) unavailable "${2:-unknown reason}" ;;
  scan) scan ;;
  -h|--help|help) usage ;;
  *) usage; exit 2 ;;
esac
