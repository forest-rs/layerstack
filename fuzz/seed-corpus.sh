#!/usr/bin/env bash
# Copies seed inputs for one fuzz target into `fuzz/corpus/<target>`: the
# minimized findings in `fuzz/regressions/<target>` and the repository's
# own layers of the formats the target reads (64 KiB or smaller). With
# `OPENUSD` naming an OpenUSD checkout, its `testSdfParsing` layers seed
# the text targets too; they are read in place, never copied into the
# repository.
#
# Usage: fuzz/seed-corpus.sh <target>
set -euo pipefail
target=$1
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
out="$root/fuzz/corpus/$target"
mkdir -p "$out"
case "$target" in
  usda | usda_round_trip) patterns=('*.usda' '*.usd') ;;
  usdc) patterns=('*.usdc' '*.usd') ;;
  usdz) patterns=('*.usdz') ;;
  stage) patterns=('*.usda' '*.usdc' '*.usd' '*.usdz') ;;
  *) patterns=() ;;
esac
copy() {
  local file=$1
  [ "$(wc -c < "$file")" -le 65536 ] || return 0
  cp "$file" "$out/$(git hash-object "$file")"
}
if [ -d "$root/fuzz/regressions/$target" ]; then
  for file in "$root/fuzz/regressions/$target"/*; do copy "$file"; done
fi
if [ ${#patterns[@]} -gt 0 ]; then
  git -C "$root" ls-files -z -- "${patterns[@]}" | while IFS= read -r -d '' file; do
    copy "$root/$file"
  done
fi
if [ -n "${OPENUSD:-}" ] && [ ${#patterns[@]} -gt 0 ] && [ "$target" != usdc ] && [ "$target" != usdz ]; then
  find "$OPENUSD/pxr/usd/sdf/testenv/testSdfParsing.testenv" -name '*.usda' | while IFS= read -r file; do
    copy "$file"
  done
fi
echo "$(find "$out" -type f | wc -l | tr -d ' ') seeds in $out"
