//! IOReport energy backend — labeled per-subsystem energy from Apple's private
//! `IOReport` framework, the same source `powermetrics` reads.
//!
//! Unlike the SMC (slow thermal proxies), IOReport's "Energy Model" group reports
//! the actual energy each subsystem (CPU / GPU / ANE) consumed between two
//! samples. Dividing a delta by its interval yields real watts — and the ANE
//! channel finally makes Neural-Engine usage directly visible.
//!
//! `IOReport` is private and has no SDK link stub, so it is loaded at runtime via
//! `dlopen`/`dlsym` (dyld resolves it from the shared cache). The sample payload
//! is walked via CoreFoundation, which is public and linked normally.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::time::{Duration, Instant};

type CFRef = *const c_void;
type CFMut = *mut c_void;
const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(alloc: CFRef, cstr: *const c_char, enc: u32) -> CFRef;
    fn CFStringGetCStringPtr(s: CFRef, enc: u32) -> *const c_char;
    fn CFStringGetCString(s: CFRef, buf: *mut c_char, size: isize, enc: u32) -> u8;
    fn CFDictionaryGetValue(d: CFRef, key: CFRef) -> CFRef;
    fn CFArrayGetCount(a: CFRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFRef, idx: isize) -> CFRef;
    fn CFRelease(cf: CFRef);
}

unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_NOW: c_int = 2;

type CopyChannelsFn = unsafe extern "C" fn(CFRef, CFRef, u64, u64, u64) -> CFMut;
type CreateSubFn = unsafe extern "C" fn(CFRef, CFMut, *mut CFMut, u64, CFRef) -> CFRef;
type CreateSamplesFn = unsafe extern "C" fn(CFRef, CFMut, CFRef) -> CFRef;
type CreateDeltaFn = unsafe extern "C" fn(CFRef, CFRef, CFRef) -> CFRef;
type ChStrFn = unsafe extern "C" fn(CFRef) -> CFRef;
type SimpleIntFn = unsafe extern "C" fn(CFRef, i32) -> i64;

/// Resolved IOReport function pointers.
struct IOReportApi {
    copy_channels: CopyChannelsFn,
    create_sub: CreateSubFn,
    create_samples: CreateSamplesFn,
    create_delta: CreateDeltaFn,
    channel_name: ChStrFn,
    unit_label: ChStrFn,
    simple_int: SimpleIntFn,
}

impl IOReportApi {
    fn load() -> Option<IOReportApi> {
        unsafe {
            let path = CString::new("/usr/lib/libIOReport.dylib").unwrap();
            let handle = dlopen(path.as_ptr(), RTLD_NOW);
            if handle.is_null() {
                return None;
            }
            let sym = |name: &str| -> Option<*mut c_void> {
                let c = CString::new(name).unwrap();
                let p = dlsym(handle, c.as_ptr());
                if p.is_null() { None } else { Some(p) }
            };
            Some(IOReportApi {
                copy_channels: std::mem::transmute::<_, CopyChannelsFn>(sym("IOReportCopyChannelsInGroup")?),
                create_sub: std::mem::transmute::<_, CreateSubFn>(sym("IOReportCreateSubscription")?),
                create_samples: std::mem::transmute::<_, CreateSamplesFn>(sym("IOReportCreateSamples")?),
                create_delta: std::mem::transmute::<_, CreateDeltaFn>(sym("IOReportCreateSamplesDelta")?),
                channel_name: std::mem::transmute::<_, ChStrFn>(sym("IOReportChannelGetChannelName")?),
                unit_label: std::mem::transmute::<_, ChStrFn>(sym("IOReportChannelGetUnitLabel")?),
                simple_int: std::mem::transmute::<_, SimpleIntFn>(sym("IOReportSimpleGetIntegerValue")?),
            })
        }
    }
}

fn cfstr(s: &str) -> CFRef {
    let c = CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) }
}

fn to_string(s: CFRef) -> String {
    if s.is_null() {
        return String::new();
    }
    unsafe {
        let p = CFStringGetCStringPtr(s, UTF8);
        if !p.is_null() {
            return CStr::from_ptr(p).to_string_lossy().into_owned();
        }
        let mut buf = [0 as c_char; 256];
        if CFStringGetCString(s, buf.as_mut_ptr(), 256, UTF8) != 0 {
            return CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
        }
    }
    String::new()
}

/// Converts an energy reading in its labeled unit to joules.
fn joules(unit: &str, value: i64) -> f64 {
    let v = value as f64;
    match unit.trim() {
        u if u.contains("mJ") => v * 1e-3,
        u if u.contains("uJ") || u.contains("µJ") => v * 1e-6,
        u if u.contains("nJ") => v * 1e-9,
        "J" => v,
        _ => v * 1e-3,
    }
}

/// A live subscription to the Energy Model channel group.
struct EnergyMeter<'a> {
    api: &'a IOReportApi,
    sub: CFRef,
    chans: CFMut,
}

impl<'a> EnergyMeter<'a> {
    fn open(api: &'a IOReportApi) -> Option<EnergyMeter<'a>> {
        unsafe {
            let group = cfstr("Energy Model");
            let chans = (api.copy_channels)(group, std::ptr::null(), 0, 0, 0);
            CFRelease(group);
            if chans.is_null() {
                return None;
            }
            let mut subbed: CFMut = std::ptr::null_mut();
            let sub = (api.create_sub)(std::ptr::null(), chans, &mut subbed, 0, std::ptr::null());
            if sub.is_null() {
                return None;
            }
            let chans = if subbed.is_null() { chans } else { subbed };
            Some(EnergyMeter { api, sub, chans })
        }
    }

    fn sample(&self) -> CFRef {
        unsafe { (self.api.create_samples)(self.sub, self.chans, std::ptr::null()) }
    }
}

/// Walks a delta-sample dictionary into (channel name, joules) pairs.
fn read_delta(api: &IOReportApi, delta: CFRef) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    unsafe {
        let key = cfstr("IOReportChannels");
        let arr = CFDictionaryGetValue(delta, key);
        CFRelease(key);
        if arr.is_null() {
            return out;
        }
        for i in 0..CFArrayGetCount(arr) {
            let ch = CFArrayGetValueAtIndex(arr, i);
            if ch.is_null() {
                continue;
            }
            let name = to_string((api.channel_name)(ch));
            let unit = to_string((api.unit_label)(ch));
            out.push((name, joules(&unit, (api.simple_int)(ch, 0))));
        }
    }
    out
}

/// Per-subsystem totals (joules over a window, or watts once scaled).
#[derive(Default, Clone, Copy)]
struct Buckets {
    cpu: f64,
    gpu: f64,
    ane: f64,
    dram: f64,
    disp: f64,
}

impl Buckets {
    fn total(&self) -> f64 {
        self.cpu + self.gpu + self.ane + self.dram + self.disp
    }
    fn add(&mut self, o: &Buckets) {
        self.cpu += o.cpu;
        self.gpu += o.gpu;
        self.ane += o.ane;
        self.dram += o.dram;
        self.disp += o.disp;
    }
    fn scale(&self, k: f64) -> Buckets {
        Buckets {
            cpu: self.cpu * k,
            gpu: self.gpu * k,
            ane: self.ane * k,
            dram: self.dram * k,
            disp: self.disp * k,
        }
    }
}

/// Sums the rollup channels only (the group also exposes per-core channels, so
/// substring-summing would double-count). Values are joules for the window.
fn bucket(channels: &[(String, f64)]) -> Buckets {
    let mut b = Buckets::default();
    for (name, j) in channels {
        match name.as_str() {
            "CPU Energy" => b.cpu += *j,
            "GPU Energy" => b.gpu += *j,
            "ANE0" | "ANE Energy" => b.ane += *j,
            "DRAM0" => b.dram += *j,
            "DISP0" => b.disp += *j,
            _ => {}
        }
    }
    b
}

/// Live per-subsystem energy meter: prints CPU / GPU / ANE watts at ~2 Hz.
pub fn run_energy() {
    let api = match IOReportApi::load() {
        Some(a) => a,
        None => {
            eprintln!("Could not load the IOReport framework.");
            std::process::exit(1);
        }
    };
    let meter = match EnergyMeter::open(&api) {
        Some(m) => m,
        None => {
            eprintln!("Could not open IOReport 'Energy Model' channels.");
            std::process::exit(1);
        }
    };
    println!("IOReport energy meter — CPU / GPU / ANE watts (ctrl-c to stop)");

    let interval = Duration::from_millis(500);
    let mut prev = meter.sample();
    let mut last = Instant::now();
    let mut announced = false;

    loop {
        std::thread::sleep(interval);
        let cur = meter.sample();
        let dt = last.elapsed().as_secs_f64();
        last = Instant::now();

        let delta = unsafe { (api.create_delta)(prev, cur, std::ptr::null()) };
        let channels = read_delta(&api, delta);

        if !announced {
            let names: Vec<&str> = channels.iter().map(|(n, _)| n.as_str()).collect();
            println!("channels: {names:?}\n");
            announced = true;
        }

        let w = bucket(&channels).scale(1.0 / dt);
        print!(
            "\rCPU {:6.2}  GPU {:6.2}  ANE {:5.2}  DRAM {:5.2}  DISP {:4.2}  │ total {:6.2} W   ",
            w.cpu, w.gpu, w.ane, w.dram, w.disp, w.total()
        );
        use std::io::Write;
        std::io::stdout().flush().ok();

        unsafe {
            CFRelease(delta);
            CFRelease(prev);
        }
        prev = cur;
    }
}

/// Profiles a child command: integrates per-subsystem energy over its runtime
/// and reports joules, the subsystem split, and the bottleneck. Run as
/// `smcprobe energy -- <command> [args...]`.
pub fn run_energy_profile(cmd: &[String]) {
    let api = match IOReportApi::load() {
        Some(a) => a,
        None => {
            eprintln!("Could not load the IOReport framework.");
            std::process::exit(1);
        }
    };
    let meter = match EnergyMeter::open(&api) {
        Some(m) => m,
        None => {
            eprintln!("Could not open IOReport 'Energy Model' channels.");
            std::process::exit(1);
        }
    };

    eprintln!("smcprobe energy · profiling: {}\n", cmd.join(" "));
    let mut child = match std::process::Command::new(&cmd[0]).args(&cmd[1..]).spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("failed to launch '{}': {e}", cmd[0]);
            std::process::exit(1);
        }
    };

    let t0 = Instant::now();
    let mut prev = meter.sample();
    let mut last = Instant::now();
    let mut total = Buckets::default();
    let mut peak_w = 0.0_f64;

    loop {
        std::thread::sleep(Duration::from_millis(200));
        let exited = matches!(child.try_wait(), Ok(Some(_)));
        let cur = meter.sample();
        let dt = last.elapsed().as_secs_f64();
        last = Instant::now();
        let delta = unsafe { (api.create_delta)(prev, cur, std::ptr::null()) };
        let window = bucket(&read_delta(&api, delta));
        total.add(&window);
        let w = window.total() / dt;
        if w > peak_w {
            peak_w = w;
        }
        unsafe {
            CFRelease(delta);
            CFRelease(prev);
        }
        prev = cur;
        if exited {
            break;
        }
    }
    unsafe { CFRelease(prev) };

    let wall = t0.elapsed().as_secs_f64();
    let tot = total.total();
    let pct = |x: f64| if tot > 0.0 { 100.0 * x / tot } else { 0.0 };
    let (label, share) = [
        ("CPU", total.cpu),
        ("GPU", total.gpu),
        ("ANE", total.ane),
        ("DRAM", total.dram),
    ]
    .into_iter()
    .fold(("CPU", 0.0), |best, (n, v)| if v > best.1 { (n, v) } else { best });
    let verdict = match label {
        "GPU" => "GPU-bound",
        "ANE" => "ANE-bound",
        "DRAM" => "memory-bound",
        _ => "CPU/compute-bound",
    };

    println!("\n─ energy profile · {} ─", cmd.join(" "));
    println!("  wall       {wall:.2} s");
    println!("  energy     {tot:.1} J   (avg {:.1} W · peak {:.1} W)", tot / wall, peak_w);
    println!(
        "  by subsystem  CPU {:.1} J · GPU {:.1} J · ANE {:.1} J · DRAM {:.1} J · DISP {:.1} J",
        total.cpu, total.gpu, total.ane, total.dram, total.disp
    );
    println!(
        "  split         CPU {:.0}% · GPU {:.0}% · ANE {:.0}% · DRAM {:.0}% · DISP {:.0}%",
        pct(total.cpu), pct(total.gpu), pct(total.ane), pct(total.dram), pct(total.disp)
    );
    println!("  → {verdict} ({label} {:.0}% of energy)", pct(share));
}
