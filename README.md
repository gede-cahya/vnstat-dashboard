# 🌐 Pure Rust vnStat Network Analytics & Live App Bandwidth Monitor

A high-performance, 100% Pure Rust Linux network traffic analytics dashboard and real-time per-application network usage monitor designed for Linux (vnStat + Hyprland / Wayland / X11) and Waybar integration.

## ✨ Key Features

- **100% Pure Rust Backend:** Zero Python scripts or heavy runtime overhead. Ultra-fast HTTP API server built with `tiny_http`.
- **vnStat Integration:** Parses `vnstat --json` to visualize hourly, daily, and monthly network bandwidth statistics (Rx / Tx / Total).
- **Per-Application Live Bandwidth Tracker:** Inspects active network sockets (`ss -tupn`) and system process I/O (`/proc/[pid]/io`) to track exact download/upload bytes per application.
- **Live Speed Badge:** Displays real-time network transfer speed (`⚡ Live Speed: 1.25 MB/s ↓ | 450 KB/s ↑`).
- **Interactive Visual Dashboard:** Beautiful dark-themed interface built with Chart.js, featuring line charts, interface selectors, and bandwidth tables.
- **Waybar Shortcut Integration:** Launch the dashboard instantly from Waybar.

## 📁 Repository Structure

```
vnstat-dashboard/
├── Cargo.toml
├── src/
│   └── main.rs
├── index.html
├── chart.umd.js
├── scripts/
│   └── show-vnstat.sh
└── README.md
```

## 🚀 Building & Installation

### Prerequisites
Ensure you have Rust, `vnstat`, `iproute2` (`ss`), and WebKitGTK installed:

```bash
# CachyOS / Arch Linux
sudo pacman -S rust cargo vnstat iproute2 webkit2gtk-4.1 gtk3
sudo systemctl enable --now vnstat
```

### Build Release Binary
```bash
cargo build --release
```
The compiled binary will be at `target/release/vnstat-gui`.

---

## ⚙️ Background Daemon Setup

Run the server daemon in background mode:
```bash
/home/cahya/2026/vnstat-dashboard/target/release/vnstat-gui --server-only &
```

Or configure a Systemd user service:

`~/.config/systemd/user/vnstat-tracker.service`:
```ini
[Unit]
Description=Pure Rust VNStat Traffic Server
After=network.target

[Service]
ExecStart=/home/cahya/2026/vnstat-dashboard/target/release/vnstat-gui --server-only
Restart=always
RestartSec=3

[Install]
WantedBy=default.target
```

Enable and start:
```bash
systemctl --user daemon-reload
systemctl --user enable --now vnstat-tracker.service
```

---

## 🌐 Waybar Integration

Add the custom module to your Waybar configuration:

`custom-vnstat.jsonc`:
```json
{
    "custom/vnstat": {
        "format": "🌐",
        "tooltip": true,
        "tooltip-format": "Klik untuk lihat Analisis Penggunaan Internet (vnStat)",
        "on-click": "/home/cahya/2026/vnstat-dashboard/scripts/show-vnstat.sh"
    }
}
```

---

## 📜 License
Licensed under the [MIT License](LICENSE).
