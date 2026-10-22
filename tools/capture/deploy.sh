#!/usr/bin/env bash
# Publish the capture list page, queries.json (from tools/netlog/panel.txt) and
# the extension zip to chunky.meenan.dev (static nginx on the `plex` host).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

python3 -I - "$repo/tools/netlog/panel.txt" "$out/queries.json" <<'EOF'
import datetime, json, sys
panel = [l.strip() for l in open(sys.argv[1]) if l.strip() and not l.lstrip().startswith('#')]
etsy = json.load(open(sys.argv[1].replace('panel.txt', 'etsy.json')))
json.dump({'version': datetime.date.today().isoformat(), 'panel': panel, 'etsy': etsy}, open(sys.argv[2], 'w'), indent=1)
EOF
# The zip matches nginx's long-expiry static rule, so bust caches per deploy.
sed "s|chunky-capture-extension.zip\"|chunky-capture-extension.zip?v=$(date +%s)\"|" "$here/site/index.html" > "$out/index.html"
(cd "$here/extension" && zip -q -r "$out/chunky-capture-extension.zip" .)

ssh plex 'mkdir -p /var/www/meenan.dev/chunky/capture'
scp -q "$out/index.html" "$out/queries.json" "$out/chunky-capture-extension.zip" plex:/var/www/meenan.dev/chunky/capture/
echo "published https://chunky.meenan.dev/capture/"
