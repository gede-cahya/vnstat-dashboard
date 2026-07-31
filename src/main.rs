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

fn get_net_dev_bytes() -> (u64, u64) {
    let mut rx = 0u64;
    let mut tx = 0u64;
    if let Ok(content) = fs::read_to_string("/proc/net/dev") {
        for line in content.lines() {
            if line.contains(':') && !line.trim().starts_with("lo") {
                if let Some(parts_str) = line.split(':').nth(1) {
                    let parts: Vec<&str> = parts_str.split_whitespace().collect();
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
        let mut prev_app_io: BTreeMap<String, u64> = BTreeMap::new();

        loop {
            thread::sleep(Duration::from_secs(1));
            let curr_rx_tx = get_net_dev_bytes();
            let delta_rx = if curr_rx_tx.0 >= prev_rx_tx.0 {
                curr_rx_tx.0 - prev_rx_tx.0
            } else {
                0
            };
            let delta_tx = if curr_rx_tx.1 >= prev_rx_tx.1 {
                curr_rx_tx.1 - prev_rx_tx.1
            } else {
                0
            };
            prev_rx_tx = curr_rx_tx;

            update_live_speed(delta_rx, delta_tx);

            if delta_rx > 0 || delta_tx > 0 {
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

                log.total_rx += delta_rx;
                log.total_tx += delta_tx;

                if total_io_delta > 0 {
                    for (app_name, io_delta) in app_io_deltas {
                        let ratio = (io_delta as f64) / (total_io_delta as f64);
                        let app_rx = ((delta_rx as f64) * ratio).round() as u64;
                        let app_tx = ((delta_tx as f64) * ratio).round() as u64;

                        let entry = log.apps.entry(app_name).or_default();
                        entry.rx += app_rx;
                        entry.tx += app_tx;
                    }
                } else {
                    let sys_entry = log.apps.entry("System / Services".to_string()).or_default();
                    sys_entry.rx += delta_rx;
                    sys_entry.tx += delta_tx;
                }

                save_app_log(&log);
            }
        }
    });
}

fn fetch_vnstat_today_totals() -> (u64, u64) {
    let mut max_rx = 0u64;
    let mut max_tx = 0u64;

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
                        if if_name.starts_with("br-") || if_name.starts_with("veth") || if_name.starts_with("docker") {
                            continue;
                        }

                        if let Some(traffic) = &iface.traffic {
                            if let Some(days) = &traffic.day {
                                for d in days {
                                    if d.date.year == curr_year && d.date.month == curr_month {
                                        if let Some(day_num) = d.date.day {
                                            if day_num == curr_day {
                                                if d.rx > max_rx {
                                                    max_rx = d.rx;
                                                    max_tx = d.tx;
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
    }
    (max_rx, max_tx)
}

fn get_synced_app_log(date_str: &str) -> DailyNetAppLog {
    let mut log = load_raw_app_log(date_str);
    let (speed_rx, speed_tx) = get_live_speed();
    log.speed_rx = speed_rx;
    log.speed_tx = speed_tx;

    let now_date = Local::now().format("%Y-%m-%d").to_string();
    if date_str == now_date {
        let (vnstat_rx, vnstat_tx) = fetch_vnstat_today_totals();
        if vnstat_rx > 0 || vnstat_tx > 0 {
            let tracked_total_rx: u64 = log.apps.values().map(|a| a.rx).sum();
            let tracked_total_tx: u64 = log.apps.values().map(|a| a.tx).sum();

            log.total_rx = vnstat_rx;
            log.total_tx = vnstat_tx;

            let mut synced_apps: BTreeMap<String, AppNetUsage> = BTreeMap::new();
            let mut allocated_rx = 0u64;
            let mut allocated_tx = 0u64;

            if tracked_total_rx > 0 || tracked_total_tx > 0 {
                for (app_name, usage) in &log.apps {
                    let rx_ratio = if tracked_total_rx > 0 {
                        (usage.rx as f64) / (tracked_total_rx as f64)
                    } else {
                        0.0
                    };
                    let scaled_rx = (vnstat_rx as f64 * rx_ratio).round() as u64;
                    allocated_rx += scaled_rx;

                    let tx_ratio = if tracked_total_tx > 0 {
                        (usage.tx as f64) / (tracked_total_tx as f64)
                    } else {
                        0.0
                    };
                    let scaled_tx = (vnstat_tx as f64 * tx_ratio).round() as u64;
                    allocated_tx += scaled_tx;

                    if scaled_rx > 0 || scaled_tx > 0 {
                        synced_apps.insert(
                            app_name.clone(),
                            AppNetUsage {
                                rx: scaled_rx,
                                tx: scaled_tx,
                            },
                        );
                    }
                }
            }

            let unallocated_rx = if vnstat_rx >= allocated_rx {
                vnstat_rx - allocated_rx
            } else {
                0
            };
            let unallocated_tx = if vnstat_tx >= allocated_tx {
                vnstat_tx - allocated_tx
            } else {
                0
            };

            if unallocated_rx > 10240 || unallocated_tx > 10240 {
                let sys_entry = synced_apps.entry("System & Background Traffic".to_string()).or_default();
                sys_entry.rx += unallocated_rx;
                sys_entry.tx += unallocated_tx;
            }

            log.apps = synced_apps;
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

        let path = PathBuf::from(&home).join(".local/share/vnstat-rust-gui").join(filename);
        let fallback_path = PathBuf::from(&home).join(".local/share/vnstat-dashboard").join(filename);

        let bytes_opt = fs::read(&path).or_else(|_| fs::read(&fallback_path));

        if let Ok(bytes) = bytes_opt {
            let content_type = if filename.ends_with(".js") {
                "application/javascript"
            } else if filename.ends_with(".css") {
                "text/css"
            } else {
                "text/html; charset=utf-8"
            };

            let response = tiny_http::Response::from_data(bytes)
                .with_header(format!("Content-Type: {}", content_type).parse::<tiny_http::Header>().unwrap())
                .with_header("Cache-Control: no-cache, no-store, must-revalidate".parse::<tiny_http::Header>().unwrap());
            let _ = request.respond(response);
        } else {
            let _ = request.respond(tiny_http::Response::from_string("404").with_status_code(404));
        }
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
