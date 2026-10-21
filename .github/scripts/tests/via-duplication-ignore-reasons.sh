#!/usr/bin/env bash
set -euo pipefail
checker=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/check-via-structural-rules.sh
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/.github/scripts" "$fixture/generated" "$fixture/target" "$fixture/.git"
cp "$checker" "$fixture/.github/scripts/"
run() { bash "$fixture/.github/scripts/check-via-structural-rules.sh" --duplication-ignore-reasons-only; }
count=0
for suffix in '' '   ' ' */' '";' ');' '---'; do
    printf '// %s%s\n' 'jscpd:ignore-start' "$suffix" > "$fixture/case.rs"
    if printf '' | run > "$fixture/log" 2>&1; then echo 'Reasonless marker passed' >&2; exit 1; fi
    grep -q 'requires a reason' "$fixture/log"
    count=$((count+1))
done
for reason in 'paired protocol fixtures' 'issue #123: intentional matrix' 'reason inside a string";'; do
    printf '// %s %s\n' 'jscpd:ignore-start' "$reason" > "$fixture/case.rs"
    run
    count=$((count+1))
done
printf 'fn nothing() {}\n' > "$fixture/case.rs"
mkdir "$fixture/ignored"
printf 'ignored/\n' > "$fixture/.gitignore"
mkdir "$fixture/contracts" "$fixture/via-core-ext"
for directory in generated target .git ignored contracts via-core-ext; do printf '// %s\n' 'jscpd:ignore-start' > "$fixture/$directory/invalid.rs"; done
run; count=$((count+1))
printf '// %s\n' 'jscpd:ignore-start' | run; count=$((count+1))
ln -s "$fixture/generated/invalid.rs" "$fixture/linked.rs"
run; count=$((count+1))
mkdir "$fixture/no-rg"
ln -s "$(command -v dirname)" "$fixture/no-rg/dirname"
if PATH="$fixture/no-rg" "$(command -v bash)" "$fixture/.github/scripts/check-via-structural-rules.sh" --duplication-ignore-reasons-only > "$fixture/log" 2>&1; then exit 1; fi
grep -q 'rg is required' "$fixture/log"; count=$((count+1))
actual_rg=$(command -v rg)
mkdir "$fixture/bad-rg"
printf '#!/usr/bin/env bash\nexit 2\n' > "$fixture/bad-rg/rg"
chmod +x "$fixture/bad-rg/rg"
if PATH="$fixture/bad-rg:$PATH" run; then echo 'Search error passed' >&2; exit 1; fi
count=$((count+1))
printf '#!/usr/bin/env bash\necho call >> %q\nexec %q "$@"\n' "$fixture/rg-calls" "$actual_rg" > "$fixture/bad-rg/rg"
PATH="$fixture/bad-rg:$PATH" run
[[ $(wc -l < "$fixture/rg-calls") == 1 ]]
count=$((count+1))
echo "$count ignore-reason fixtures passed; 0 failed."
