use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

static EMBEDDED_INDEX_HTML: &str = include_str!("../index.html");
static EMBEDDED_CHART_JS: &[u8] = include_bytes!("../chart.umd.js");

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct AppNetUsage {
    rx: u64,
    tx: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct DailyNetAppLog {
    date: String,
    total_rx: u64,
    total_tx: u64,
    speed_rx: u64,
    speed_tx: u64,
    #[serde(default)]
    interface: String,
    apps: BTreeMap<String, AppNetUsage>,
}

#[derive(Deserialize, Debug)]
struct VnstatDate {
    year: i32,
    month: u32,
    day: Option<u32>,
}

#[derive(Deserialize, Debug)]
struct VnstatTrafficItem {
    date: VnstatDate,
    rx: u64,
    tx: u64,
}

#[derive(Deserialize, Debug)]
struct VnstatInterface {
    name: Option<String>,
    traffic: Option<VnstatTrafficContainer>,
}

#[derive(Deserialize, Debug)]
struct VnstatTrafficContainer {
    day: Option<Vec<VnstatTrafficItem>>,
}

#[derive(Deserialize, Debug)]
struct VnstatJson {
    interfaces: Option<Vec<VnstatInterface>>,
}

fn get_base_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = PathBuf::from(home).join(".local/share/vnstat-dashboard");
    fs::create_dir_all(&dir).ok();
    dir
}

fn is_physical_interface(name: &str) -> bool {
    let n = name.trim();
    if n.starts_with("lo")
        || n.starts_with("br-")
        || n.starts_with("docker")
        || n.starts_with("veth")
        || n.starts_with("virbr")
        || n.starts_with("tun")
        || n.starts_with("tap")
        || n.starts_with("wg")
        || n.starts_with("Cloudflare")
        || n.starts_with("warp")
    {
        return false;
    }
    let device_path = format!("/sys/class/net/{}/device", n);
    if std::path::Path::new(&device_path).exists() {
        return true;
    }
    n.starts_with("en") || n.starts_with("wl") || n.starts_with("eth") || n.starts_with("wlan")
}

fn get_default_interface() -> Option<String> {
    if let Ok(content) = fs::read_to_string("/proc/net/route") {
        let mut fallback = None;
        for line in content.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 4 {
                let iface = fields[0];
                let dest = fields[1];
                if dest == "00000000" {
                    if is_physical_interface(iface) {
                        return Some(iface.to_string());
                    }
                    if fallback.is_none() && !iface.starts_with("lo") && !iface.starts_with("docker") && !iface.starts_with("br-") {
                        fallback = Some(iface.to_string());
                    }
                }
            }
        }
        if fallback.is_some() {
            return fallback;
        }
    }
    // Fallback: inspect /sys/class/net for active physical interface
    if let Ok(entries) = fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            if let Ok(name) = entry.file_name().into_string() {
                if is_physical_interface(&name) {
                    let oper_path = format!("/sys/class/net/{}/operstate", name);
                    if let Ok(state) = fs::read_to_string(oper_path) {
                        if state.trim() == "up" {
                            return Some(name);
                        }
                    }
                }
            }
        }
    }
    None
}

fn get_net_dev_bytes() -> (u64, u64) {
    let target_iface = get_default_interface();
    let mut rx = 0u64;
    let mut tx = 0u64;
    if let Ok(content) = fs::read_to_string("/proc/net/dev") {
        for line in content.lines() {
            if let Some((if_name, stats)) = line.split_once(':') {
                let name = if_name.trim();
                let should_count = match &target_iface {
                    Some(target) => name == target,
                    None => is_physical_interface(name),
                };

                if should_count {
                    let parts: Vec<&str> = stats.split_whitespace().collect();
                    if parts.len() >= 9 {
                        if let (Ok(r), Ok(t)) = (parts[0].parse::<u64>(), parts[8].parse::<u64>()) {
                            rx += r;
                            tx += t;
                        }
                    }
                }
            }
        }
    }
    (rx, tx)
}

fn get_active_net_app_pids() -> BTreeMap<u32, String> {
    let mut pids = BTreeMap::new();
    if let Ok(output) = Command::new("ss").args(["-tupn"]).output() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.contains("users:") {
                let mut start = 0;
                while let Some(idx) = line[start..].find("\"") {
                    let actual_idx = start + idx + 1;
                    if let Some(end_idx) = line[actual_idx..].find("\"") {
                        let app_name = &line[actual_idx..actual_idx + end_idx];
                        let clean_name = app_name.trim().to_string();

                        let remainder = &line[actual_idx + end_idx..];
                        if let Some(pid_idx) = remainder.find("pid=") {
                            let pid_str_start = pid_idx + 4;
                            let pid_str: String = remainder[pid_str_start..]
                                .chars()
                                .take_while(|c| c.is_ascii_digit())
                                .collect();
                            if let Ok(pid) = pid_str.parse::<u32>() {
                                if !clean_name.is_empty()
                                    && clean_name != "vnstat-gui"
                                    && clean_name != "ss"
                                    && !clean_name.contains('/')
                                {
                                    pids.insert(pid, clean_name);
                                }
                            }
                        }
                        start = actual_idx + end_idx + 1;
                    } else {
                        break;
                    }
                }
            }
        }
    }
    pids
}

fn get_proc_io_bytes(pid: u32) -> u64 {
    let path = format!("/proc/{}/io", pid);
    if let Ok(content) = fs::read_to_string(path) {
        let mut read_b = 0u64;
        let mut write_b = 0u64;
        for line in content.lines() {
            if line.starts_with("read_bytes:") {
                if let Some(val) = line.split_whitespace().nth(1) {
                    read_b = val.parse().unwrap_or(0);
                }
            } else if line.starts_with("write_bytes:") {
                if let Some(val) = line.split_whitespace().nth(1) {
                    write_b = val.parse().unwrap_or(0);
                }
            }
        }
        return read_b + write_b;
    }
    0
}

fn load_raw_app_log(date_str: &str) -> DailyNetAppLog {
    let path = get_base_dir().join(format!("net_apps_{}.json", date_str));
    if path.exists() {
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(log) = serde_json::from_str::<DailyNetAppLog>(&content) {
                return log;
            }
        }
    }
    DailyNetAppLog {
        date: date_str.to_string(),
        total_rx: 0,
        total_tx: 0,
        speed_rx: 0,
        speed_tx: 0,
        interface: get_default_interface().unwrap_or_else(|| "enp7s0".to_string()),
        apps: BTreeMap::new(),
    }
}

fn save_app_log(log: &DailyNetAppLog) {
    let path = get_base_dir().join(format!("net_apps_{}.json", log.date));
    let tmp_path = path.with_extension("tmp");
    if let Ok(json) = serde_json::to_string_pretty(log) {
        if fs::write(&tmp_path, json).is_ok() {
            fs::rename(tmp_path, path).ok();
        }
    }
}

struct LiveState {
    speed_rx: u64,
    speed_tx: u64,
}

static LIVE_STATE: Mutex<Option<LiveState>> = Mutex::new(None);

fn update_live_speed(rx_speed: u64, tx_speed: u64) {
    if let Ok(mut state) = LIVE_STATE.lock() {
        *state = Some(LiveState {
            speed_rx: rx_speed,
            speed_tx: tx_speed,
        });
    }
}

fn get_live_speed() -> (u64, u64) {
    if let Ok(state) = LIVE_STATE.lock() {
        if let Some(ref s) = *state {
            return (s.speed_rx, s.speed_tx);
        }
    }
    (0, 0)
}

fn start_net_app_tracker_thread() {
    thread::spawn(|| {
        let mut prev_rx_tx = get_net_dev_bytes();
        let mut prev_time = std::time::Instant::now();
        let mut prev_app_io: BTreeMap<String, u64> = BTreeMap::new();

        loop {
            thread::sleep(Duration::from_secs(1));
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(prev_time).as_secs_f64();
            prev_time = now;

            let curr_rx_tx = get_net_dev_bytes();
            let raw_delta_rx = if curr_rx_tx.0 >= prev_rx_tx.0 {
                curr_rx_tx.0 - prev_rx_tx.0
            } else {
                0
            };
            let raw_delta_tx = if curr_rx_tx.1 >= prev_rx_tx.1 {
                curr_rx_tx.1 - prev_rx_tx.1
            } else {
                0
            };
            prev_rx_tx = curr_rx_tx;

            let speed_rx = if elapsed > 0.0 {
                ((raw_delta_rx as f64) / elapsed).round() as u64
            } else {
                raw_delta_rx
            };
            let speed_tx = if elapsed > 0.0 {
                ((raw_delta_tx as f64) / elapsed).round() as u64
            } else {
                raw_delta_tx
            };

            update_live_speed(speed_rx, speed_tx);

            if raw_delta_rx > 0 || raw_delta_tx > 0 {
                let app_pids = get_active_net_app_pids();
                let mut curr_app_io: BTreeMap<String, u64> = BTreeMap::new();

                for (pid, app_name) in app_pids {
                    let io_bytes = get_proc_io_bytes(pid);
                    *curr_app_io.entry(app_name).or_default() += io_bytes;
                }

                // Calculate IO delta per app
                let mut app_io_deltas: BTreeMap<String, u64> = BTreeMap::new();
                let mut total_io_delta = 0u64;

                for (app_name, curr_bytes) in &curr_app_io {
                    let prev_bytes = prev_app_io.get(app_name).cloned().unwrap_or(0);
                    let delta = if *curr_bytes >= prev_bytes {
                        *curr_bytes - prev_bytes
                    } else {
                        0
                    };
                    if delta > 0 {
                        app_io_deltas.insert(app_name.clone(), delta);
                        total_io_delta += delta;
                    }
                }
                prev_app_io = curr_app_io;

                let date_str = Local::now().format("%Y-%m-%d").to_string();
                let mut log = load_raw_app_log(&date_str);

                log.total_rx += raw_delta_rx;
                log.total_tx += raw_delta_tx;

                if total_io_delta > 0 {
                    for (app_name, io_delta) in app_io_deltas {
                        let ratio = (io_delta as f64) / (total_io_delta as f64);
                        let app_rx = ((raw_delta_rx as f64) * ratio).round() as u64;
                        let app_tx = ((raw_delta_tx as f64) * ratio).round() as u64;

                        let entry = log.apps.entry(app_name).or_default();
                        entry.rx += app_rx;
                        entry.tx += app_tx;
                    }
                } else {
                    let sys_entry = log.apps.entry("System / Services".to_string()).or_default();
                    sys_entry.rx += raw_delta_rx;
                    sys_entry.tx += raw_delta_tx;
                }

                save_app_log(&log);
            }
        }
    });
}

fn fetch_vnstat_today_totals() -> (u64, u64) {
    let target_iface = get_default_interface();
    let mut target_rx = 0u64;
    let mut target_tx = 0u64;

    if let Ok(output) = Command::new("vnstat").arg("--json").output() {
        if output.status.success() {
            if let Ok(vjson) = serde_json::from_slice::<VnstatJson>(&output.stdout) {
                if let Some(ifaces) = vjson.interfaces {
                    let now = Local::now();
                    let curr_year = now.format("%Y").to_string().parse::<i32>().unwrap_or(0);
                    let curr_month = now.format("%m").to_string().parse::<u32>().unwrap_or(0);
                    let curr_day = now.format("%d").to_string().parse::<u32>().unwrap_or(0);

                    for iface in ifaces {
                        let if_name = iface.name.unwrap_or_default();
                        let is_match = match &target_iface {
                            Some(t) => &if_name == t,
                            None => is_physical_interface(&if_name),
                        };
                        if !is_match {
                            continue;
                        }

                        if let Some(traffic) = &iface.traffic {
                            if let Some(days) = &traffic.day {
                                for d in days {
                                    if d.date.year == curr_year && d.date.month == curr_month {
                                        if let Some(day_num) = d.date.day {
                                            if day_num == curr_day {
                                                target_rx = d.rx;
                                                target_tx = d.tx;
                                                return (target_rx, target_tx);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    (target_rx, target_tx)
}

fn get_synced_app_log(date_str: &str) -> DailyNetAppLog {
    let mut log = load_raw_app_log(date_str);
    let (speed_rx, speed_tx) = get_live_speed();
    log.speed_rx = speed_rx;
    log.speed_tx = speed_tx;
    if log.interface.is_empty() {
        log.interface = get_default_interface().unwrap_or_else(|| "enp7s0".to_string());
    }

    let now_date = Local::now().format("%Y-%m-%d").to_string();
    if date_str == now_date {
        let (vnstat_rx, vnstat_tx) = fetch_vnstat_today_totals();
        let tracked_total_rx: u64 = log.apps.values().map(|a| a.rx).sum();
        let tracked_total_tx: u64 = log.apps.values().map(|a| a.tx).sum();

        let effective_rx = std::cmp::max(log.total_rx, std::cmp::max(vnstat_rx, tracked_total_rx));
        let effective_tx = std::cmp::max(log.total_tx, std::cmp::max(vnstat_tx, tracked_total_tx));

        log.total_rx = effective_rx;
        log.total_tx = effective_tx;

        if effective_rx > tracked_total_rx || effective_tx > tracked_total_tx {
            let unallocated_rx = effective_rx.saturating_sub(tracked_total_rx);
            let unallocated_tx = effective_tx.saturating_sub(tracked_total_tx);
            if unallocated_rx > 10240 || unallocated_tx > 10240 {
                let sys_entry = log.apps.entry("System & Background Traffic".to_string()).or_default();
                sys_entry.rx = sys_entry.rx.saturating_add(unallocated_rx);
                sys_entry.tx = sys_entry.tx.saturating_add(unallocated_tx);
            }
        }
    }

    log
}

fn handle_http_request(request: tiny_http::Request) {
    let url = request.url().to_string();
    if url.starts_with("/api/data") {
        let json_data = match Command::new("vnstat").arg("--json").output() {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).to_string()
            }
            _ => "{\"error\":\"Failed to execute vnstat\"}".to_string(),
        };

        let response = tiny_http::Response::from_string(json_data)
            .with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap())
            .with_header("Cache-Control: no-cache, no-store, must-revalidate".parse::<tiny_http::Header>().unwrap())
            .with_header("Access-Control-Allow-Origin: *".parse::<tiny_http::Header>().unwrap());
        let _ = request.respond(response);
    } else if url.starts_with("/api/app_data") {
        let date_str = if let Some(idx) = url.find("date=") {
            url[idx + 5..].split('&').next().unwrap_or("")
        } else {
            ""
        };
        let target_date = if date_str.is_empty() {
            Local::now().format("%Y-%m-%d").to_string()
        } else {
            date_str.to_string()
        };

        let app_log = get_synced_app_log(&target_date);
        let json_data = serde_json::to_string(&app_log).unwrap_or_else(|_| "{}".to_string());

        let response = tiny_http::Response::from_string(json_data)
            .with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap())
            .with_header("Cache-Control: no-cache, no-store, must-revalidate".parse::<tiny_http::Header>().unwrap())
            .with_header("Access-Control-Allow-Origin: *".parse::<tiny_http::Header>().unwrap());
        let _ = request.respond(response);
    } else {
        let home = std::env::var("HOME").unwrap_or_default();
        let clean_url = url.split('?').next().unwrap_or(&url);
        let filename = if clean_url == "/" || clean_url == "/index.html" {
            "index.html"
        } else {
            clean_url.trim_start_matches('/')
        };

        let repo_path = PathBuf::from(&home).join("2026/vnstat-dashboard").join(filename);
        let local_cur = PathBuf::from(filename);
        let path = PathBuf::from(&home).join(".local/share/vnstat-rust-gui").join(filename);
        let fallback_path = PathBuf::from(&home).join(".local/share/vnstat-dashboard").join(filename);

        let bytes_opt = fs::read(&repo_path)
            .or_else(|_| fs::read(&local_cur))
            .or_else(|_| fs::read(&path))
            .or_else(|_| fs::read(&fallback_path));

        let (content_type, bytes) = if let Ok(b) = bytes_opt {
            let ct = if filename.ends_with(".js") {
                "application/javascript"
            } else if filename.ends_with(".css") {
                "text/css"
            } else {
                "text/html; charset=utf-8"
            };
            (ct, b)
        } else if filename == "index.html" {
            ("text/html; charset=utf-8", EMBEDDED_INDEX_HTML.as_bytes().to_vec())
        } else if filename == "chart.umd.js" {
            ("application/javascript", EMBEDDED_CHART_JS.to_vec())
        } else {
            let response = tiny_http::Response::from_string("404").with_status_code(404);
            let _ = request.respond(response);
            return;
        };

        let response = tiny_http::Response::from_data(bytes)
            .with_header(format!("Content-Type: {}", content_type).parse::<tiny_http::Header>().unwrap())
            .with_header("Cache-Control: no-cache, no-store, must-revalidate".parse::<tiny_http::Header>().unwrap())
            .with_header("Access-Control-Allow-Origin: *".parse::<tiny_http::Header>().unwrap());
        let _ = request.respond(response);
    }
}

fn start_embedded_http_server() {
    start_net_app_tracker_thread();

    thread::spawn(|| {
        let server = match tiny_http::Server::http("127.0.0.1:9876") {
            Ok(s) => s,
            Err(_) => return, // Server already running
        };

        for request in server.incoming_requests() {
            handle_http_request(request);
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
    std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    std::env::set_var("GDK_BACKEND", "x11");

    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--server-only" {
        println!("Pure Rust IO-Weighted VNStat + App Tracker Server starting on 127.0.0.1:9876...");
        start_net_app_tracker_thread();

        let server = tiny_http::Server::http("127.0.0.1:9876")?;
        for request in server.incoming_requests() {
            handle_http_request(request);
        }
        return Ok(());
    }

    start_embedded_http_server();
    thread::sleep(Duration::from_millis(150));

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Analytics Penggunaan Internet")
        .with_inner_size(tao::dpi::LogicalSize::new(980.0, 740.0))
        .build(&event_loop)?;

    let _webview = WebViewBuilder::new()
        .with_url("http://127.0.0.1:9876/index.html")
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}
