#!/bin/sh
# Lightweight secret scan used when gitleaks is not installed.
#   secret-scan.sh            scan every tracked and untracked (non-ignored) file
#   secret-scan.sh --staged   scan staged changes only
# Exits non-zero when something that looks like a credential is found.
set -eu
cd "$(git rev-parse --show-toplevel)"
PATTERNS='github_pat_[A-Za-z0-9_]{20,}|gh[pousr]_[A-Za-z0-9]{30,}|xox[baprs]-[A-Za-z0-9-]{10,}|discord(app)?\.com/api/webhooks/[0-9]+/[A-Za-z0-9_-]+|hooks\.slack\.com/services/T[A-Za-z0-9]+/|-----BEGIN [A-Z ]*PRIVATE KEY|[0-9]{8,10}:AA[A-Za-z0-9_-]{30,}|mysql://[^:"@ ]+:[^@"$ {]{6,}@|AKIA[0-9A-Z]{16}'
# Documentation placeholders and test fixtures that are not credentials.
ALLOW='example\.invalid|@127\.0\.0\.1|@host[:/ ]|user:password@|ci-only-password|change-me'
found=0
if [ "${1:-}" = "--staged" ]; then
    hits=$(git diff --cached -U0 | grep -E '^\+' | grep -E -e "$PATTERNS" | grep -v -E -e "$ALLOW" || true)
    if [ -n "$hits" ]; then
        echo "possible secret in staged changes (first characters shown):" >&2
        printf '%s\n' "$hits" | grep -E -o -e "$PATTERNS" | cut -c1-16 | sed 's/$/.../' >&2
        found=1
    fi
else
    # This script only contains the patterns themselves.
    for f in $(git ls-files --cached --others --exclude-standard | grep -v '^scripts/secret-scan.sh$'); do
        [ -f "$f" ] || continue
        hits=$(grep -I -n -E -e "$PATTERNS" "$f" 2>/dev/null | grep -v -E -e "$ALLOW" || true)
        if [ -n "$hits" ]; then
            printf '%s\n' "$hits" | cut -d: -f1 | while read -r line; do echo "possible secret in $f:$line" >&2; done
            found=1
        fi
    done
fi
if [ "$found" -eq 0 ]; then echo "secret scan: no findings"; fi
exit "$found"
