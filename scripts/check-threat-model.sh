#!/usr/bin/env bash
# Every test the threat model cites as evidence must exist. Run in CI: a renamed or deleted test
# must not leave a mitigation claiming evidence it no longer has.
set -euo pipefail
cd "$(dirname "$0")/.."
doc="${THREAT_MODEL_DOC:-docs/THREAT_MODEL.md}"
status=0
# Evidence cells are the last column of the tables: pull backticked snake_case identifiers there.
names=$(grep -E '^\|' "$doc" | awk -F'|' 'NF>=5 {print $(NF-1)}' | grep -oE '`[a-z][a-z0-9_]+`' | tr -d '`' | sort -u)
checked=0
for n in $names; do
  # Skip things that are not test names (constants, config keys, words in backticks).
  if ! grep -rqE --include="*.rs" --exclude-dir=target --exclude-dir=node_modules "fn ${n}\b" crates bench apps 2>/dev/null; then
    if grep -qE "\`${n}\`" "$doc" && [[ "$n" == *_* ]]; then
      echo "MISSING test cited in THREAT_MODEL.md: $n"; status=1
    fi
  else
    checked=$((checked+1))
  fi
done
echo "threat-model traceability: ${checked} cited tests found"
exit $status
