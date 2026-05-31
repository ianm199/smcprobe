//! The portable, hardware-independent snapshot.
//!
//! A [`Snapshot`] is built by applying a [`Profile`] to whatever a
//! [`SensorSource`] returns. The twin UI and the analysis consume only this —
//! they never see raw keys or know which OS produced them. That is the seam
//! that makes the rest of the tool portable.

use std::collections::HashMap;

use crate::profile::Profile;
use crate::sensors::SensorSource;

/// A logical component with a temperature and optional power draw.
#[derive(Default)]
pub struct Subsystem {
    pub temp: Option<f32>,
    pub power: Option<f32>,
}

/// One power-delivery rail with its V/I/P readings.
pub struct Rail {
    pub key: String,
    pub v: Option<f32>,
    pub i: Option<f32>,
    pub p: f32,
}

/// A full normalized reading of the machine at one instant.
pub struct Snapshot {
    pub cpu_p: Subsystem,
    pub cpu_e: Subsystem,
    pub gpu: Subsystem,
    pub dram: Subsystem,
    pub ssd: Subsystem,
    pub battery: Subsystem,
    pub hotspot: Option<f32>,
    pub fans: Vec<Option<f32>>,
    pub total_power: Option<f32>,
    pub cluster0: Option<f32>,
    pub cluster1: Option<f32>,
    pub grid_p: Vec<(String, f32)>,
    pub grid_e: Vec<(String, f32)>,
    pub grid_g: Vec<(String, f32)>,
    pub rails: Vec<Rail>,
    pub adapter_v: Option<f32>,
    pub adapter_i: Option<f32>,
    pub charger_present: bool,
    pub backlight_w: Option<f32>,
    pub ane_temp: Option<f32>,
}

impl Snapshot {
    /// Reads every key the profile references in one batch, then reduces them
    /// into logical subsystems.
    pub fn build(src: &dyn SensorSource, p: &Profile) -> Self {
        let mut want: Vec<String> = Vec::new();
        add(&mut want, &p.cpu_p_temp);
        add(&mut want, &p.cpu_e_temp);
        add(&mut want, &p.gpu_temp);
        add(&mut want, &p.hotspot);
        add(&mut want, &p.dram_temp);
        add(&mut want, &p.ssd_temp);
        add(&mut want, &p.battery_temp);
        add(&mut want, &p.fans);
        add(&mut want, &p.cpu_clusters);
        want.push(p.dram_power.clone());
        want.push(p.system_power.clone());
        add(&mut want, &p.adapter_voltage);
        add(&mut want, &p.adapter_current);
        want.push(p.backlight_power.clone());
        want.push(p.backlight_current.clone());
        add(&mut want, &p.ane_temp);
        for s in &p.rails {
            want.push(format!("V{s}"));
            want.push(format!("I{s}"));
            want.push(format!("P{s}"));
        }
        let refs: Vec<&str> = want.iter().map(|s| s.as_str()).collect();
        let m = src.read(&refs);

        let clusters: Vec<f32> = p.cpu_clusters.iter().filter_map(|k| m.get(k).copied()).collect();
        let cpu_power = if clusters.is_empty() {
            None
        } else {
            Some(clusters.iter().sum())
        };

        let rails = p
            .rails
            .iter()
            .filter_map(|s| {
                m.get(format!("P{s}").as_str()).map(|&p| Rail {
                    key: s.clone(),
                    v: m.get(format!("V{s}").as_str()).copied(),
                    i: m.get(format!("I{s}").as_str()).copied(),
                    p,
                })
            })
            .collect();

        Snapshot {
            cpu_p: Subsystem {
                temp: avg(&m, &p.cpu_p_temp),
                power: cpu_power,
            },
            cpu_e: Subsystem {
                temp: avg(&m, &p.cpu_e_temp),
                power: None,
            },
            gpu: Subsystem {
                temp: avg(&m, &p.gpu_temp),
                power: None,
            },
            dram: Subsystem {
                temp: avg(&m, &p.dram_temp),
                power: m.get(&p.dram_power).copied(),
            },
            ssd: Subsystem {
                temp: avg(&m, &p.ssd_temp),
                power: None,
            },
            battery: Subsystem {
                temp: avg(&m, &p.battery_temp),
                power: None,
            },
            hotspot: maxv(&m, &p.hotspot),
            fans: p.fans.iter().map(|k| m.get(k).copied()).collect(),
            total_power: m.get(&p.system_power).copied(),
            cluster0: p.cpu_clusters.first().and_then(|k| m.get(k).copied()),
            cluster1: p.cpu_clusters.get(1).and_then(|k| m.get(k).copied()),
            grid_p: grid(&m, &p.cpu_p_temp),
            grid_e: grid(&m, &p.cpu_e_temp),
            grid_g: grid(&m, &p.gpu_temp),
            rails,
            adapter_v: avg(&m, &p.adapter_voltage),
            adapter_i: avg(&m, &p.adapter_current),
            charger_present: avg(&m, &p.adapter_voltage).is_some_and(|v| v > 1.0),
            backlight_w: m.get(&p.backlight_power).copied(),
            ane_temp: avg(&m, &p.ane_temp),
        }
    }

    /// Serializes to the JSON shape the twin UI consumes.
    pub fn to_json(&self) -> String {
        format!(
            concat!(
                r#"{{"cpu_p":{{"temp":{},"power":{}}},"cpu_e":{{"temp":{}}},"#,
                r#""hotspot":{},"gpu":{{"temp":{}}},"dram":{{"temp":{},"power":{}}},"#,
                r#""ssd":{{"temp":{}}},"battery":{{"temp":{}}},"#,
                r#""fans":[{{"rpm":{}}},{{"rpm":{}}}],"#,
                r#""power":{{"total":{},"cluster0":{},"cluster1":{}}},"#,
                r#""adapter":{{"v":{},"i":{},"present":{}}},"backlight":{{"power":{}}},"#,
                r#""ane":{{"temp":{}}},"#,
                r#""grid_p":{},"grid_e":{},"grid_g":{},"rails":{}}}"#
            ),
            opt(self.cpu_p.temp),
            opt(self.cpu_p.power),
            opt(self.cpu_e.temp),
            opt(self.hotspot),
            opt(self.gpu.temp),
            opt(self.dram.temp),
            opt(self.dram.power),
            opt(self.ssd.temp),
            opt(self.battery.temp),
            opt(self.fans.first().copied().flatten()),
            opt(self.fans.get(1).copied().flatten()),
            opt(self.total_power),
            opt(self.cluster0),
            opt(self.cluster1),
            opt(self.adapter_v),
            opt(self.adapter_i),
            self.charger_present,
            opt(self.backlight_w),
            opt(self.ane_temp),
            grid_json(&self.grid_p),
            grid_json(&self.grid_e),
            grid_json(&self.grid_g),
            rails_json(&self.rails),
        )
    }
}

fn add(want: &mut Vec<String>, keys: &[String]) {
    for k in keys {
        want.push(k.clone());
    }
}

fn avg(m: &HashMap<String, f32>, keys: &[String]) -> Option<f32> {
    let vals: Vec<f32> = keys.iter().filter_map(|k| m.get(k).copied()).collect();
    if vals.is_empty() {
        None
    } else {
        Some(vals.iter().sum::<f32>() / vals.len() as f32)
    }
}

fn maxv(m: &HashMap<String, f32>, keys: &[String]) -> Option<f32> {
    keys.iter()
        .filter_map(|k| m.get(k).copied())
        .fold(None, |acc, v| Some(acc.map_or(v, |a: f32| a.max(v))))
}

fn grid(m: &HashMap<String, f32>, keys: &[String]) -> Vec<(String, f32)> {
    keys.iter()
        .filter_map(|k| m.get(k).map(|v| (k.clone(), *v)))
        .collect()
}

fn opt(v: Option<f32>) -> String {
    match v {
        Some(x) => format!("{x:.2}"),
        None => "null".to_string(),
    }
}

fn grid_json(cells: &[(String, f32)]) -> String {
    let items: Vec<String> = cells
        .iter()
        .map(|(k, v)| format!("{{\"k\":\"{k}\",\"v\":{v:.2}}}"))
        .collect();
    format!("[{}]", items.join(","))
}

fn rails_json(rails: &[Rail]) -> String {
    let items: Vec<String> = rails
        .iter()
        .map(|r| {
            format!(
                "{{\"k\":\"{}\",\"v\":{},\"i\":{},\"p\":{:.3}}}",
                r.key,
                opt(r.v),
                opt(r.i),
                r.p
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}
