#!/usr/bin/env bash
set -euo pipefail
checker=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/check-via-structural-rules.sh
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/.github/scripts" "$fixture/generated" "$fixture/target"
git init -q "$fixture"
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
git -C "$fixture" add case.rs
printf '// %s\n' 'jscpd:ignore-start' > "$fixture/case.rs"
if run > "$fixture/log" 2>&1; then echo 'Tracked working-tree violation passed' >&2; exit 1; fi
grep -q 'requires a reason' "$fixture/log"; count=$((count+1))
git -C "$fixture" add case.rs
printf 'fn nothing() {}\n' > "$fixture/case.rs"
run; count=$((count+1))
mkdir "$fixture/ignored"
printf 'ignored/\n' > "$fixture/.gitignore"
mkdir "$fixture/contracts" "$fixture/via-core-ext"
mkdir -p "$fixture/nested/generated" "$fixture/nested/target"
for directory in generated target nested/generated nested/target .git ignored contracts via-core-ext; do printf '// %s\n' 'jscpd:ignore-start' > "$fixture/$directory/invalid.rs"; done
run; count=$((count+1))
printf '// %s\n' 'jscpd:ignore-start' | run; count=$((count+1))
ln -s "$fixture/generated/invalid.rs" "$fixture/linked.rs"
run; count=$((count+1))
mkdir "$fixture/bad-git"
printf '#!/usr/bin/env bash\necho "Synthetic git grep failure" >&2\nexit 128\n' > "$fixture/bad-git/git"
chmod +x "$fixture/bad-git/git"
status=0
PATH="$fixture/bad-git:$PATH" run > "$fixture/log" 2>&1 || status=$?
[[ $status == 2 ]]
grep -q 'Synthetic git grep failure' "$fixture/log"
count=$((count+1))
echo "$count ignore-reason fixtures passed; 0 failed."
