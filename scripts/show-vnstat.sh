#!/usr/bin/env bash

# Ensure 100% Pure Rust VNStat Server is active
if ! pgrep -f "vnstat-gui --server-only" > /dev/null; then
    nohup ~/.local/share/vnstat-rust-gui/target/release/vnstat-gui --server-only > /dev/null 2>&1 &
    sleep 0.3
fi

# Launch VNStat App Dashboard Window with timestamp to force fresh fetch and eliminate cached 0% items
TS=$(date +%s)
chromium --app="http://127.0.0.1:9876/index.html?t=${TS}" --user-data-dir="/tmp/vnstat-dashboard-chrome-profile" > /dev/null 2>&1 &
