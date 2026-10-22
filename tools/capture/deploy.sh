#!/usr/bin/env bash
# Publish the capture list page, capture.json (built by build_list.py from
# tools/netlog/panel.txt and etsy.json) and the extension zip to
# chunky.meenan.dev (static nginx on the `plex` host).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

python3 -I "$here/build_list.py" "$repo/tools/netlog/panel.txt" "$repo/tools/netlog/etsy.json" "$out/capture.json"
# The zip matches nginx's long-expiry static rule, so bust caches per deploy.
sed "s|chunky-capture-extension.zip\"|chunky-capture-extension.zip?v=$(date +%s)\"|" "$here/site/index.html" > "$out/index.html"
(cd "$here/extension" && zip -q -r "$out/chunky-capture-extension.zip" .)

ssh plex 'mkdir -p /var/www/meenan.dev/chunky/capture'
scp -q "$out/index.html" "$out/capture.json" "$out/chunky-capture-extension.zip" plex:/var/www/meenan.dev/chunky/capture/
echo "published https://chunky.meenan.dev/capture/"
