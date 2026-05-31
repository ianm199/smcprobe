//! smcprobe — reverse-engineer the Apple Silicon SMC; live twin is the demo.
//!
//! Reads a machine's raw sensors through a [`sensors::SensorSource`] backend,
//! maps them to logical subsystems with a [`profile::Profile`], and reduces them
//! into a portable [`model::Snapshot`] that drives the browser twin and the CLI
//! views. Today the only backend is the Apple SMC; the layers above it are
//! hardware-independent.
//!
//! Modes:
//!   (no args)  live terminal dashboard, 1 Hz
//!   serve      browser digital twin at http://127.0.0.1:8077
//!   energy     live per-subsystem watts (CPU/GPU/ANE/DRAM) via IOReport
//!   once       one dashboard frame, then exit
//!   scan       dump every decodable key with its value and type
//!   json       one JSON snapshot of all numeric keys (used by the probe harness)
//!   schema     key -> type map (used by the probe harness)

mod ioreport;
mod model;
mod profile;
mod sensors;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use model::Snapshot;
use profile::Profile;
use sensors::{AppleSmc, SensorSource};

const TWIN_HTML: &str = include_str!("twin.html");

/// Reads this machine's model identifier (e.g. `Mac15,11`) from sysctl.
fn detect_model() -> String {
    std::process::Command::new("sysctl")
        .args(["-n", "hw.model"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Loads the sensor profile for the running machine. Uses a mapped profile if
/// one exists for this model, otherwise builds a generic one from the keys the
/// machine exposes — so the twin works on any Apple Silicon Mac out of the box.
fn load_profile(src: &dyn SensorSource) -> Profile {
    let model = detect_model();
    if let Some(p) = Profile::for_model(&model) {
        return p;
    }
    eprintln!(
        "No mapped profile for '{model}' (mapped: {:?}) — using a generic Apple Silicon \
         profile from prefix heuristics. Run the harness for a verified profiles/{model}.profile.",
        Profile::bundled_models()
    );
    Profile::generic(&src.schema())
}

fn main() {
    // The energy modes use IOReport, not the SMC — handle them before opening one.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("energy") {
        if let Some(pos) = args.iter().position(|a| a == "--") {
            let cmd = &args[pos + 1..];
            if !cmd.is_empty() {
                ioreport::run_energy_profile(cmd);
                return;
            }
        }
        ioreport::run_energy();
        return;
    }

    let src = match AppleSmc::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Could not open AppleSMC (error {e}).");
            std::process::exit(1);
        }
    };
    let profile = load_profile(&src);
    eprintln!("smcprobe · model {} · {} rails, {} P-core sensors mapped",
        profile.model, profile.rails.len(), profile.cpu_p_temp.len());

    match std::env::args().nth(1).as_deref() {
        Some("serve") => serve(),
        Some("scan") => scan(&src),
        Some("json") => print_values(&src),
        Some("schema") => print_schema(&src),
        Some("once") => render_frame(&Snapshot::build(&src, &profile)),
        _ => {
            let started = Instant::now();
            loop {
                print!("\x1b[2J\x1b[H");
                render_frame(&Snapshot::build(&src, &profile));
                println!("  uptime {:>4}s   ctrl-c to quit", started.elapsed().as_secs());
                std::io::Write::flush(&mut std::io::stdout()).ok();
                std::thread::sleep(Duration::from_millis(1000));
            }
        }
    }
}

/// Serves the twin page and an SSE sensor stream; one SMC handle per connection.
fn serve() {
    let addr = "127.0.0.1:8077";
    let listener = TcpListener::bind(addr).expect("failed to bind 127.0.0.1:8077");
    println!("Digital twin live at http://{addr}  (ctrl-c to stop)");
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || handle_conn(stream));
    }
}

fn handle_conn(mut stream: TcpStream) {
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = match req.split_whitespace().nth(1) {
        Some(p) => p,
        None => return,
    };

    if path == "/stream" {
        let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                       Cache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
        if stream.write_all(headers.as_bytes()).is_err() {
            return;
        }
        let src = match AppleSmc::open() {
            Ok(s) => s,
            Err(_) => return,
        };
        let profile = load_profile(&src);
        loop {
            let msg = format!("data: {}\n\n", Snapshot::build(&src, &profile).to_json());
            if stream.write_all(msg.as_bytes()).is_err() || stream.flush().is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    } else {
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            TWIN_HTML.len(),
            TWIN_HTML
        );
        let _ = stream.write_all(resp.as_bytes());
    }
}

/// Dumps every decodable key with its value and type, grouped into temps/power.
fn scan(src: &dyn SensorSource) {
    let schema = src.schema();
    let types: HashMap<&str, &str> = schema.iter().map(|(k, t)| (k.as_str(), t.as_str())).collect();
    let keys: Vec<&str> = schema.iter().map(|(k, _)| k.as_str()).collect();
    let values = src.read(&keys);

    let mut temps: Vec<(&str, f32)> = Vec::new();
    let mut power: Vec<(&str, f32)> = Vec::new();
    for (k, v) in &values {
        if k.starts_with('T') && types.get(k.as_str()) == Some(&"flt") && *v > 0.0 && *v < 150.0 {
            temps.push((k, *v));
        } else if k.starts_with('P') && types.get(k.as_str()) == Some(&"flt") {
            power.push((k, *v));
        }
    }
    temps.sort_by(|a, b| a.0.cmp(b.0));
    power.sort_by(|a, b| a.0.cmp(b.0));

    println!("{} keys exposed.\n\n=== Temperatures (°C) ===", schema.len());
    for (k, v) in &temps {
        println!("  {k:<6} {v:>8.2} °C");
    }
    println!("\n=== Power (W) ===");
    for (k, v) in &power {
        println!("  {k:<6} {v:>8.2} W");
    }
    println!("\n{} temperature and {} power sensors.", temps.len(), power.len());
}

/// Prints one JSON object of every numeric key to its value.
fn print_values(src: &dyn SensorSource) {
    let schema = src.schema();
    let keys: Vec<&str> = schema.iter().map(|(k, _)| k.as_str()).collect();
    let values = src.read(&keys);
    let items: Vec<String> = values.iter().map(|(k, v)| format!("\"{k}\":{v:.4}")).collect();
    println!("{{{}}}", items.join(","));
}

/// Prints one JSON object of every numeric key to its decode type.
fn print_schema(src: &dyn SensorSource) {
    let items: Vec<String> = src
        .schema()
        .iter()
        .map(|(k, t)| format!("\"{k}\":\"{t}\""))
        .collect();
    println!("{{{}}}", items.join(","));
}

fn temp_color(c: f32) -> &'static str {
    if c < 50.0 {
        "\x1b[32m"
    } else if c < 75.0 {
        "\x1b[33m"
    } else {
        "\x1b[31m"
    }
}

fn bar(value: f32, min: f32, max: f32, width: usize) -> String {
    let frac = ((value - min) / (max - min)).clamp(0.0, 1.0);
    let filled = (frac * width as f32).round() as usize;
    (0..width).map(|i| if i < filled { '█' } else { '░' }).collect()
}

/// Renders one terminal-dashboard frame from a normalized snapshot.
fn render_frame(s: &Snapshot) {
    let row = |label: &str, temp: Option<f32>, power: Option<f32>| {
        let t = match temp {
            Some(v) => format!("{}{:>6.1} °C\x1b[0m {}{}\x1b[0m", temp_color(v), v, temp_color(v), bar(v, 20.0, 105.0, 18)),
            None => "     —".to_string(),
        };
        let p = power.map(|v| format!("  {v:>6.1} W")).unwrap_or_default();
        println!("│ {label:<10} {t}{p}");
    };

    println!("┌─ M3 Max live sensors ─────────────────────────────────────┐");
    if let Some(h) = s.hotspot {
        println!(
            "│ hottest P-core {}{:>5.1}°\x1b[0m   die hotspot {}{:>5.1}°\x1b[0m   power {:>5.1} W",
            temp_color(s.cpu_p.temp.unwrap_or(0.0)),
            s.cpu_p.temp.unwrap_or(0.0),
            temp_color(h),
            h,
            s.total_power.unwrap_or(0.0)
        );
    }
    println!("├───────────────────────────────────────────────────────────┤");
    row("P-cores", s.cpu_p.temp, s.cpu_p.power);
    row("E-cores", s.cpu_e.temp, None);
    row("GPU", s.gpu.temp, None);
    row("DRAM", s.dram.temp, s.dram.power);
    row("SSD", s.ssd.temp, None);
    row("Battery", s.battery.temp, None);
    println!("├─ fans ────────────────────────────────────────────────────┤");
    for (i, f) in s.fans.iter().enumerate() {
        println!("│ Fan {i}      {:>6} rpm", f.map(|v| format!("{v:.0}")).unwrap_or_else(|| "—".into()));
    }
    let mut rails: Vec<&model::Rail> = s.rails.iter().collect();
    rails.sort_by(|a, b| b.p.total_cmp(&a.p));
    println!("├─ top power rails (W) ─────────────────────────────────────┤");
    for r in rails.iter().take(5) {
        println!("│ {:<6} {:>6.2} W  \x1b[36m{}\x1b[0m", r.key, r.p, bar(r.p, 0.0, 35.0, 18));
    }
    println!("└───────────────────────────────────────────────────────────┘");
}
