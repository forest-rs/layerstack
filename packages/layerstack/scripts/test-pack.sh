#!/usr/bin/env bash
# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT

set -euo pipefail
package=$(cd "$(dirname "$0")/.." && pwd)
consumer=$(mktemp -d)
trap 'rm -rf "$consumer"' EXIT

cd "$package"
npm pack --pack-destination "$consumer"
cp scripts/pack-consumer.mts "$consumer/"

cd "$consumer"
npm init -y >/dev/null
npm install --ignore-scripts --no-audit --no-fund ./forest-rs-layerstack-*.tgz
"$package/node_modules/.bin/tsc" --noEmit --strict --module nodenext \
  --target es2022 --lib es2022,esnext.disposable,dom --typeRoots "$package/node_modules/@types" \
  pack-consumer.mts
node pack-consumer.mts
