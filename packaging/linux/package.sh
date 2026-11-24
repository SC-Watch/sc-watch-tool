#!/usr/bin/env bash
# The Linux test build: one folder people unpack and run (portable.txt keeps
# the data beside the programs), as a tar.gz.
#
#   dist/sc-watch/
#   dist/sc-watch-rust-<commit>-linux-x86_64.tar.gz
#
# Run from the repository root after
#   cargo build --release --locked -p scw-app -p scw-watch -p scw-ui
# and, for the licences, cargo about generate (licenses/third-party.html).
set -euo pipefail
cd "$(dirname "$0")/../.."

commit=$(git rev-parse --short HEAD)
out=dist/sc-watch
rm -rf dist
mkdir -p "$out/licenses"
for bin in sc-watch sc-watch-watcher sc-watch-ui; do
  install -m 755 "target/release/$bin" "$out/$bin"
done
cp starter_list.json "$out/"
cp LICENSE "$out/LICENSE.txt"
if [ -f dist-licenses.html ]; then cp dist-licenses.html "$out/licenses/third-party.html"; fi
sed "s/@COMMIT@/$commit/" packaging/linux/README.txt >"$out/README.txt"
cat >"$out/portable.txt" <<'TXT'
While this file is here, sc-watch keeps everything it saves (its database,
settings, audits and logs) in the "data" folder next to it, so the whole
tool is this one folder.

Delete this file to keep that data in ~/.local/share/sc-watch instead.
TXT

archive="dist/sc-watch-rust-$commit-linux-x86_64.tar.gz"
tar -czf "$archive" -C dist sc-watch
echo "$archive"
