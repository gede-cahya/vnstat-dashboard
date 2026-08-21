#!/usr/bin/env bash

# Ensure 100% Pure Rust VNStat Server is active
if ! pgrep -f "vnstat-gui --server-only" > /dev/null; then
    nohup ~/.local/share/vnstat-rust-gui/target/release/vnstat-gui --server-only > /dev/null 2>&1 &
    sleep 0.3
fi

PROFILE_DIR="/tmp/vnstat-dashboard-chrome-profile"
mkdir -p "$PROFILE_DIR"
touch "$PROFILE_DIR/First Run"

# Launch VNStat App Dashboard Window with flags to skip ToS/Fre and enable floating window
TS=$(date +%s)
chromium \
    --app="http://127.0.0.1:9876/index.html?t=${TS}" \
    --class="VnstatDashboard" \
    --user-data-dir="$PROFILE_DIR" \
    --no-first-run \
    --no-default-browser-check \
    --disable-session-crashed-bubble \
    --disable-infobars \
    --password-store=basic \
    --disable-features=Translate,OptimizationHints \
    > /dev/null 2>&1 &
