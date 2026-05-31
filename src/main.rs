//! Reads live sensors from the Apple System Management Controller (SMC).
//!
//! The SMC is a dedicated microcontroller on the logic board that owns the
//! machine's analog reality: temperatures, fan speeds, power rails, the
//! ambient-light and lid sensors. macOS reaches it through IOKit by opening a
//! user-client connection to the `AppleSMC` driver and issuing a single
//! struct-based method call (`KERNEL_INDEX_SMC`) whose meaning is selected by a
//! command byte inside the payload.
//!
//! The SMC speaks in four-character keys (FourCC), e.g. `TC0P` for a CPU-die
//! temperature or `#KEY` for "how many keys do you have". Each key carries a
//! type tag — also a FourCC — describing how to decode its bytes. Two endian
//! conventions live side by side and are the classic trap: integer keys are
//! big-endian, while `flt ` keys are little-endian IEEE-754. This program
//! enumerates every key the controller exposes and decodes the thermal and
//! power sensors.
//!
//! Modes:
//!   (no args)  live dashboard, refreshing once a second
//!   once       print a single dashboard frame and exit (for scripted capture)
//!   scan       dump every decodable thermal/power key in the controller's table

use std::ffi::CString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::raw::{c_char, c_void};
use std::time::{Duration, Instant};

/// The digital-twin web page, embedded so the binary is self-contained.
const TWIN_HTML: &str = include_str!("twin.html");

type KernReturn = i32;
type MachPort = u32;
type IoService = MachPort;
type IoConnect = MachPort;

/// The single IOKit method selector the SMC user-client understands.
const KERNEL_INDEX_SMC: u32 = 2;
/// Command byte: copy the raw bytes of `key` into the reply.
const SMC_CMD_READ_BYTES: u8 = 5;
/// Command byte: return the key located at `data32` in the controller's table.
const SMC_CMD_READ_INDEX: u8 = 8;
/// Command byte: return the size and type tag of `key` without its value.
const SMC_CMD_READ_KEYINFO: u8 = 9;

#[link(name = "IOKit", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: MachPort, matching: *mut c_void) -> IoService;
    fn IOServiceOpen(
        service: IoService,
        owning_task: MachPort,
        type_: u32,
        connect: *mut IoConnect,
    ) -> KernReturn;
    fn IOServiceClose(connect: IoConnect) -> KernReturn;
    fn IOObjectRelease(object: IoService) -> KernReturn;
    fn IOConnectCallStructMethod(
        connection: IoConnect,
        selector: u32,
        input: *const c_void,
        input_cnt: usize,
        output: *mut c_void,
        output_cnt: *mut usize,
    ) -> KernReturn;
}

unsafe extern "C" {
    static mach_task_self_: MachPort;
}

/// Firmware version block the SMC echoes back; unused here but part of the
/// fixed wire layout the kernel expects.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SmcVersion {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

/// Power-limit negotiation block; present only to preserve struct layout.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SmcPLimitData {
    version: u16,
    length: u16,
    cpu_plimit: u32,
    gpu_plimit: u32,
    mem_plimit: u32,
}

/// Describes a key's payload: how many bytes it is and how to decode them.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SmcKeyInfo {
    data_size: u32,
    data_type: u32,
    data_attributes: u8,
}

/// The exact 80-byte request/response packet exchanged with the SMC.
///
/// `#[repr(C)]` pins the field order and padding so the bytes Rust sends match
/// what the kernel driver parses. Getting this layout wrong is the canonical
/// FFI bug — the size assertion below is the cheap insurance.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SmcKeyData {
    key: u32,
    vers: SmcVersion,
    plimit: SmcPLimitData,
    key_info: SmcKeyInfo,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<SmcKeyData>() == 80);

/// An open user-client connection to the `AppleSMC` driver.
struct SmcConnection {
    conn: IoConnect,
}

impl SmcConnection {
    /// Matches the `AppleSMC` service in the IORegistry and opens a connection
    /// to it. The matched service object is released immediately; only the
    /// connection handle needs to outlive this call.
    fn open() -> Result<Self, KernReturn> {
        let name = CString::new("AppleSMC").unwrap();
        let conn = unsafe {
            let matching = IOServiceMatching(name.as_ptr());
            if matching.is_null() {
                return Err(-1);
            }
            let service = IOServiceGetMatchingService(0, matching);
            if service == 0 {
                return Err(-2);
            }
            let mut conn: IoConnect = 0;
            let kr = IOServiceOpen(service, mach_task_self_, 0, &mut conn);
            IOObjectRelease(service);
            if kr != 0 {
                return Err(kr);
            }
            conn
        };
        Ok(Self { conn })
    }

    /// Issues one SMC struct call and returns the reply packet, surfacing both
    /// the IOKit transport status and the controller's own `result` byte as
    /// errors so the caller never decodes a failed read.
    fn call(&self, input: &SmcKeyData) -> Result<SmcKeyData, KernReturn> {
        let mut output = SmcKeyData::default();
        let mut out_size = std::mem::size_of::<SmcKeyData>();
        let kr = unsafe {
            IOConnectCallStructMethod(
                self.conn,
                KERNEL_INDEX_SMC,
                input as *const _ as *const c_void,
                std::mem::size_of::<SmcKeyData>(),
                &mut output as *mut _ as *mut c_void,
                &mut out_size,
            )
        };
        if kr != 0 {
            return Err(kr);
        }
        if output.result != 0 {
            return Err(output.result as KernReturn);
        }
        Ok(output)
    }

    /// Returns the total number of keys the controller currently exposes by
    /// reading the meta-key `#KEY`.
    fn key_count(&self) -> Result<u32, KernReturn> {
        let (info, bytes) = self.read_key(fourcc("#KEY"))?;
        Ok(decode_be_uint(&bytes, info.data_size as usize) as u32)
    }

    /// Resolves the key stored at `index` in the controller's internal table.
    fn key_at_index(&self, index: u32) -> Result<u32, KernReturn> {
        let input = SmcKeyData {
            data8: SMC_CMD_READ_INDEX,
            data32: index,
            ..Default::default()
        };
        Ok(self.call(&input)?.key)
    }

    /// Reads a key's type/size metadata followed by its raw value bytes.
    fn read_key(&self, key: u32) -> Result<(SmcKeyInfo, [u8; 32]), KernReturn> {
        let info_req = SmcKeyData {
            key,
            data8: SMC_CMD_READ_KEYINFO,
            ..Default::default()
        };
        let info = self.call(&info_req)?.key_info;

        let mut value_req = SmcKeyData {
            key,
            data8: SMC_CMD_READ_BYTES,
            ..Default::default()
        };
        value_req.key_info.data_size = info.data_size;
        Ok((info, self.call(&value_req)?.bytes))
    }

    /// Reads a key by name and decodes it to a single number, or `None` if the
    /// key is absent or not a numeric type. No fallback value is invented: an
    /// unreadable sensor is simply omitted from the display.
    fn read_numeric(&self, key: &str) -> Option<f32> {
        let (info, bytes) = self.read_key(fourcc(key)).ok()?;
        decode_f32(&info, &bytes)
    }
}

impl Drop for SmcConnection {
    /// Closing the user client when the value goes out of scope is the whole
    /// reason this is a struct: the kernel resource is released deterministically
    /// even on an early error return.
    fn drop(&mut self) {
        unsafe {
            IOServiceClose(self.conn);
        }
    }
}

/// Packs a four-character key into the big-endian `u32` the SMC expects.
fn fourcc(s: &str) -> u32 {
    let b = s.as_bytes();
    ((b[0] as u32) << 24) | ((b[1] as u32) << 16) | ((b[2] as u32) << 8) | (b[3] as u32)
}

/// Unpacks a big-endian FourCC `u32` back into its four printable characters.
fn fourcc_to_string(code: u32) -> String {
    code.to_be_bytes().iter().map(|&b| b as char).collect()
}

/// Decodes a big-endian unsigned integer of the given byte width.
fn decode_be_uint(bytes: &[u8], size: usize) -> u64 {
    bytes[..size].iter().fold(0u64, |acc, &b| (acc << 8) | b as u64)
}

/// Renders a key's bytes into a human value using its SMC type tag.
///
/// The endian split is the lesson: `flt ` payloads are little-endian IEEE-754
/// floats, every integer and fixed-point type is big-endian. `sp78` is a signed
/// 8.8 fixed-point format the SMC historically used for temperatures.
fn decode_value(info: &SmcKeyInfo, bytes: &[u8]) -> String {
    let type_tag = fourcc_to_string(info.data_type);
    let size = info.data_size as usize;
    match type_tag.as_str() {
        "flt " => {
            let v = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            format!("{v:.2}")
        }
        "ui8 " | "ui16" | "ui32" => decode_be_uint(bytes, size).to_string(),
        "sp78" => {
            let raw = i16::from_be_bytes([bytes[0], bytes[1]]);
            format!("{:.2}", raw as f32 / 256.0)
        }
        "fpe2" => {
            let raw = u16::from_be_bytes([bytes[0], bytes[1]]);
            format!("{:.2}", raw as f32 / 4.0)
        }
        _ => bytes[..size]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Decodes a numeric SMC value to `f32`, honoring the per-type endianness.
///
/// `flt ` is little-endian IEEE-754; the integer and fixed-point types are
/// big-endian. Anything else returns `None` rather than guessing.
fn decode_f32(info: &SmcKeyInfo, bytes: &[u8]) -> Option<f32> {
    let size = info.data_size as usize;
    match fourcc_to_string(info.data_type).as_str() {
        "flt " => Some(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        "ui8 " | "ui16" | "ui32" => Some(decode_be_uint(bytes, size) as f32),
        "sp78" => Some(i16::from_be_bytes([bytes[0], bytes[1]]) as f32 / 256.0),
        "fpe2" => Some(u16::from_be_bytes([bytes[0], bytes[1]]) as f32 / 4.0),
        _ => None,
    }
}

/// Renders a value as a fixed-width bar scaled between `min` and `max`.
fn bar(value: f32, min: f32, max: f32, width: usize) -> String {
    let frac = ((value - min) / (max - min)).clamp(0.0, 1.0);
    let filled = (frac * width as f32).round() as usize;
    let mut s = String::with_capacity(width);
    for i in 0..width {
        s.push(if i < filled { '█' } else { '░' });
    }
    s
}

/// ANSI color escape chosen by temperature band: green / yellow / red.
fn temp_color(c: f32) -> &'static str {
    if c < 50.0 {
        "\x1b[32m"
    } else if c < 75.0 {
        "\x1b[33m"
    } else {
        "\x1b[31m"
    }
}

/// Curated thermal keys with human labels. Apple documents none of these; the
/// labels are inferred from observed behavior under load.
const TEMP_KEYS: &[(&str, &str)] = &[
    ("TPD0", "CPU P-core die 0"),
    ("TPD1", "CPU P-core die 1"),
    ("TPD2", "CPU P-core die 2"),
    ("TPD3", "CPU P-core die 3"),
    ("TPD4", "CPU P-core die 4"),
    ("TPD5", "CPU P-core die 5"),
    ("TCMz", "CPU hotspot"),
    ("TB0T", "Battery pack"),
    ("TAOL", "Airflow / ambient"),
];

/// Curated power-rail keys with human labels.
const POWER_KEYS: &[(&str, &str)] = &[
    ("PSTR", "System total"),
    ("PZC0", "CPU cluster 0"),
    ("PZC1", "CPU cluster 1"),
    ("PHPS", "Rail PHPS"),
    ("PPMC", "Rail PPMC"),
];

/// Fan tachometer keys (actual RPM).
const FAN_KEYS: &[(&str, &str)] = &[("F0Ac", "Fan 0"), ("F1Ac", "Fan 1")];

/// Draws one full dashboard frame from a fresh round of sensor reads.
fn render_frame(smc: &SmcConnection) {
    let hottest = TEMP_KEYS
        .iter()
        .filter(|(k, _)| k.starts_with("TPD"))
        .filter_map(|(k, _)| smc.read_numeric(k))
        .fold(f32::MIN, f32::max);
    let system_w = smc.read_numeric("PSTR");

    println!("┌─ M3 Max live sensors ──────────────────────────────────────┐");
    if hottest > f32::MIN {
        println!(
            "│ hottest P-core: {}{:>6.1} °C\x1b[0m     system power: {:>6.1} W       │",
            temp_color(hottest),
            hottest,
            system_w.unwrap_or(0.0)
        );
    }
    println!("├─ Temperatures (20–105 °C) ─────────────────────────────────┤");
    for (key, label) in TEMP_KEYS {
        if let Some(v) = smc.read_numeric(key) {
            println!(
                "│ {:<18} {}{:>6.1} °C\x1b[0m {}{}\x1b[0m │",
                label,
                temp_color(v),
                v,
                temp_color(v),
                bar(v, 20.0, 105.0, 20)
            );
        }
    }
    println!("├─ Power rails (0–60 W) ─────────────────────────────────────┤");
    for (key, label) in POWER_KEYS {
        if let Some(v) = smc.read_numeric(key) {
            println!(
                "│ {:<18} {:>6.2} W  \x1b[36m{}\x1b[0m │",
                label,
                v,
                bar(v, 0.0, 60.0, 20)
            );
        }
    }
    for (key, label) in FAN_KEYS {
        if let Some(v) = smc.read_numeric(key) {
            println!("│ {:<18} {:>6.0} rpm{:>23}│", label, v, "");
        }
    }
    println!("└────────────────────────────────────────────────────────────┘");
}

/// Enumerates and prints every decodable thermal and power key in the table.
fn scan(smc: &SmcConnection) {
    let count = smc.key_count().expect("failed to read #KEY");
    println!("AppleSMC reports {count} keys.\n");

    let mut temperatures: Vec<Reading> = Vec::new();
    let mut power: Vec<Reading> = Vec::new();

    for index in 0..count {
        let key_code = match smc.key_at_index(index) {
            Ok(k) => k,
            Err(_) => continue,
        };
        let (info, bytes) = match smc.read_key(key_code) {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let key = fourcc_to_string(key_code);
        let type_tag = fourcc_to_string(info.data_type);
        let reading = Reading {
            key: key.clone(),
            type_tag: type_tag.clone(),
            value: decode_value(&info, &bytes),
        };

        if key.starts_with('T') && type_tag == "flt " {
            if let Ok(v) = reading.value.parse::<f32>() {
                if v > 0.0 && v < 150.0 {
                    temperatures.push(reading);
                }
            }
        } else if key.starts_with('P') && type_tag == "flt " {
            power.push(reading);
        }
    }

    temperatures.sort_by(|a, b| a.key.cmp(&b.key));
    power.sort_by(|a, b| a.key.cmp(&b.key));

    println!("=== Temperature sensors (°C) ===");
    for r in &temperatures {
        println!("  {:<6} {:>8} °C   [{}]", r.key, r.value, r.type_tag);
    }
    println!("\n=== Power sensors (W) ===");
    for r in &power {
        println!("  {:<6} {:>8} W    [{}]", r.key, r.value, r.type_tag);
    }
    println!(
        "\nDecoded {} thermal and {} power sensors live from the controller.",
        temperatures.len(),
        power.len()
    );
}

/// Prints every decodable numeric key as a single-line JSON object mapping
/// key to value. This is the time-series sample format the probe harness logs.
fn dump_values_json(smc: &SmcConnection) {
    let count = smc.key_count().expect("failed to read #KEY");
    let mut out = String::from("{");
    let mut first = true;
    for index in 0..count {
        let Ok(code) = smc.key_at_index(index) else {
            continue;
        };
        let Ok((info, bytes)) = smc.read_key(code) else {
            continue;
        };
        let Some(v) = decode_f32(&info, &bytes) else {
            continue;
        };
        if !v.is_finite() {
            continue;
        }
        let key = fourcc_to_string(code);
        if !key.bytes().all(|b| b.is_ascii_graphic()) {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!("\"{key}\":{v:.4}"));
    }
    out.push('}');
    println!("{out}");
}

/// Prints a one-time key-to-type map so the harness records each sensor's
/// decode format alongside the value stream.
fn dump_schema_json(smc: &SmcConnection) {
    let count = smc.key_count().expect("failed to read #KEY");
    let mut out = String::from("{");
    let mut first = true;
    for index in 0..count {
        let Ok(code) = smc.key_at_index(index) else {
            continue;
        };
        let Ok((info, bytes)) = smc.read_key(code) else {
            continue;
        };
        if decode_f32(&info, &bytes).is_none() {
            continue;
        }
        let key = fourcc_to_string(code);
        if !key.bytes().all(|b| b.is_ascii_graphic()) {
            continue;
        }
        let type_tag = fourcc_to_string(info.data_type);
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!("\"{key}\":\"{}\"", type_tag.trim_end()));
    }
    out.push('}');
    println!("{out}");
}

/// Averages the readable values among a set of keys, or `None` if none read.
fn avg(smc: &SmcConnection, keys: &[&str]) -> Option<f32> {
    let vals: Vec<f32> = keys.iter().filter_map(|k| smc.read_numeric(k)).collect();
    if vals.is_empty() {
        None
    } else {
        Some(vals.iter().sum::<f32>() / vals.len() as f32)
    }
}

/// Returns the maximum readable value among a set of keys.
fn max_of(smc: &SmcConnection, keys: &[&str]) -> Option<f32> {
    keys.iter()
        .filter_map(|k| smc.read_numeric(k))
        .fold(None, |acc, v| Some(acc.map_or(v, |a: f32| a.max(v))))
}

/// Formats an optional reading as a JSON number or `null`.
fn opt(v: Option<f32>) -> String {
    match v {
        Some(x) => format!("{x:.2}"),
        None => "null".to_string(),
    }
}

/// Every P-core thermal sensor key, in stable order, for the heatmap grid.
const P_KEYS: &[&str] = &[
    "Tp04", "Tp05", "Tp06", "Tp0C", "Tp0D", "Tp0E", "Tp0K", "Tp0L", "Tp0M", "Tp0R", "Tp0S",
    "Tp0T", "Tp0U", "Tp0V", "Tp0W", "Tp0a", "Tp0b", "Tp0c", "Tp0g", "Tp0h", "Tp0i", "Tp0m",
    "Tp0n", "Tp0o", "Tp0u", "Tp0v", "Tp0w", "Tp0y", "Tp0z", "Tp10", "Tp16", "Tp17", "Tp18",
    "Tp1E", "Tp1F", "Tp1G", "Tp1I", "Tp1J", "Tp1K", "Tp1Q", "Tp1R", "Tp1S", "Tp1g", "Tp1h",
    "Tp1i", "Tp1o", "Tp1p", "Tp1q", "Tp1x", "Tp1y", "Tp1z", "Tp25", "Tp26", "Tp27", "Tp29",
    "Tp2A", "Tp2B", "Tp2H", "Tp2I", "Tp2J", "Tp2P", "Tp2Q", "Tp2R", "Tp2X", "Tp2Y", "Tp2Z",
    "Tp2f", "Tp2g", "Tp2h", "Tp2j", "Tp2k", "Tp2l", "Tp2r", "Tp2s", "Tp2t", "Tp2z", "Tp30",
    "Tp31", "Tp33", "Tp34", "Tp35", "Tp3B", "Tp3C", "Tp3D", "Tp3O", "Tp3P", "Tp3S", "Tp3T",
    "Tp3W", "Tp3X", "Tp3a", "Tp3b", "Tp3e", "Tp3f", "Tp3i", "Tp3j",
];

/// Every E-core thermal sensor key, in stable order.
const E_KEYS: &[&str] = &[
    "Te04", "Te05", "Te06", "Te0K", "Te0L", "Te0M", "Te0P", "Te0Q", "Te0S", "Te0T",
];

/// Every GPU thermal sensor key, in stable order (confirmed by the GPU probe).
const G_KEYS: &[&str] = &[
    "Tg00", "Tg01", "Tg04", "Tg05", "Tg0C", "Tg0D", "Tg0K", "Tg0L", "Tg0y", "Tg0z", "Tg16",
    "Tg17", "Tg1E", "Tg1F", "Tg1s", "Tg1t", "Tg1x", "Tg1y", "Tg21", "Tg22", "Tg29", "Tg2A",
    "Tg2H", "Tg2I", "Tg33", "Tg34", "Tg3B", "Tg3C", "Tg3J", "Tg3K", "Tg3x", "Tg3y",
];

/// Builds a JSON array of `{k,v}` for every readable key, for a heatmap grid.
fn grid_json(smc: &SmcConnection, keys: &[&str]) -> String {
    let cells: Vec<String> = keys
        .iter()
        .filter_map(|k| smc.read_numeric(k).map(|v| format!("{{\"k\":\"{k}\",\"v\":{v:.2}}}")))
        .collect();
    format!("[{}]", cells.join(","))
}

/// Power-delivery rails that expose voltage, current, and power for the same
/// suffix, enabling a P = V x I cross-check in the power tree.
const RAIL_SUFFIX: &[&str] = &[
    "C00", "C01", "C02", "C03", "C10", "C11", "C12", "C13", "C20", "C21", "C22", "C23", "C32",
    "C40", "C42", "C43", "E0b", "E1b", "P0b", "P1b", "P1l", "P2b", "P2l", "P3b", "P3l", "P4l",
    "P5b", "P5l", "P6b", "P7b", "P8b", "P9b", "R0b", "R0l", "R1b", "R1l", "R2b", "R3b", "R4b",
    "R5b", "R6b", "R7b", "R8b", "R9b", "SVR", "b0f",
];

/// Builds a JSON array of `{k,v,i,p}` for every rail with a readable power value.
fn rails_json(smc: &SmcConnection) -> String {
    let cells: Vec<String> = RAIL_SUFFIX
        .iter()
        .filter_map(|s| {
            let p = smc.read_numeric(&format!("P{s}"))?;
            let v = smc.read_numeric(&format!("V{s}"));
            let i = smc.read_numeric(&format!("I{s}"));
            Some(format!("{{\"k\":\"{s}\",\"v\":{},\"i\":{},\"p\":{p:.3}}}", opt(v), opt(i)))
        })
        .collect();
    format!("[{}]", cells.join(","))
}

/// Builds one grouped sensor snapshot: per-subsystem aggregates plus the full
/// per-sensor grids for the P-core, E-core, and GPU heatmaps.
fn snapshot_json(smc: &SmcConnection) -> String {
    let hotspot = max_of(smc, &["TCMb", "TCMz"]);
    let dram_t = avg(smc, &["TRD0", "TRD1", "TRD3", "TRD4", "TRD8", "TRDb"]);
    let dram_p = smc.read_numeric("PMVC");
    let ssd = avg(smc, &["TH0a", "TH0b", "TH0x"]);
    let batt = avg(smc, &["TB0T", "TB1T", "TB2T"]);
    let f0 = smc.read_numeric("F0Ac");
    let f1 = smc.read_numeric("F1Ac");
    let total = smc.read_numeric("PSTR");
    let c0 = smc.read_numeric("PZC0");
    let c1 = smc.read_numeric("PZC1");
    let cpu_power = match (c0, c1) {
        (Some(a), Some(b)) => Some(a + b),
        _ => None,
    };
    format!(
        concat!(
            r#"{{"cpu_p":{{"temp":{},"power":{}}},"cpu_e":{{"temp":{}}},"#,
            r#""hotspot":{},"gpu":{{"temp":{}}},"dram":{{"temp":{},"power":{}}},"#,
            r#""ssd":{{"temp":{}}},"battery":{{"temp":{}}},"#,
            r#""fans":[{{"rpm":{}}},{{"rpm":{}}}],"#,
            r#""power":{{"total":{},"cluster0":{},"cluster1":{}}},"#,
            r#""grid_p":{},"grid_e":{},"grid_g":{},"rails":{}}}"#
        ),
        opt(avg(smc, P_KEYS)), opt(cpu_power), opt(avg(smc, E_KEYS)),
        opt(hotspot), opt(avg(smc, G_KEYS)),
        opt(dram_t), opt(dram_p), opt(ssd), opt(batt), opt(f0), opt(f1),
        opt(total), opt(c0), opt(c1),
        grid_json(smc, P_KEYS), grid_json(smc, E_KEYS), grid_json(smc, G_KEYS),
        rails_json(smc)
    )
}

/// Serves the digital-twin page and a Server-Sent-Events sensor stream on a
/// local port. Each connection gets its own SMC handle and thread.
fn serve() {
    let addr = "127.0.0.1:8077";
    let listener = TcpListener::bind(addr).expect("failed to bind 127.0.0.1:8077");
    println!("Digital twin live at http://{addr}  (ctrl-c to stop)");
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || handle_conn(stream));
    }
}

/// Handles a single HTTP request: the page at `/`, the event stream at `/stream`.
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
        let smc = match SmcConnection::open() {
            Ok(c) => c,
            Err(_) => return,
        };
        loop {
            let msg = format!("data: {}\n\n", snapshot_json(&smc));
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

/// A decoded sensor reading ready for display.
struct Reading {
    key: String,
    type_tag: String,
    value: String,
}

fn main() {
    let smc = match SmcConnection::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Could not open AppleSMC (error {e}).");
            std::process::exit(1);
        }
    };

    match std::env::args().nth(1).as_deref() {
        Some("scan") => scan(&smc),
        Some("json") => dump_values_json(&smc),
        Some("schema") => dump_schema_json(&smc),
        Some("serve") => {
            drop(smc);
            serve();
        }
        Some("once") => render_frame(&smc),
        _ => {
            let started = Instant::now();
            loop {
                print!("\x1b[2J\x1b[H");
                render_frame(&smc);
                println!("  uptime {:>4}s   ctrl-c to quit", started.elapsed().as_secs());
                std::io::Write::flush(&mut std::io::stdout()).ok();
                std::thread::sleep(Duration::from_millis(1000));
            }
        }
    }
}
