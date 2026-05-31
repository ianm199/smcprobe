//! The semantic map: which raw sensor keys correspond to which logical
//! subsystem, **as data keyed by machine model**.
//!
//! A `Profile` is loaded for the running machine's `hw.model`. Profiles live as
//! plain text files (`profiles/<model>.profile`) so adding a new Mac is dropping
//! in a file — no recompile. Known profiles are also embedded in the binary so
//! it runs standalone. New Macs get a profile from the mapping harness.
//!
//! Labels are empirical/correlational, not vendor ground truth.

use std::collections::HashMap;
use std::fs;

/// Maps a machine's raw sensor keys onto logical subsystems.
pub struct Profile {
    pub model: String,
    pub cpu_p_temp: Vec<String>,
    pub cpu_e_temp: Vec<String>,
    pub gpu_temp: Vec<String>,
    pub hotspot: Vec<String>,
    pub dram_temp: Vec<String>,
    pub dram_power: String,
    pub ssd_temp: Vec<String>,
    pub battery_temp: Vec<String>,
    pub fans: Vec<String>,
    pub system_power: String,
    pub cpu_clusters: Vec<String>,
    pub rails: Vec<String>,
    pub adapter_voltage: Vec<String>,
    pub adapter_current: Vec<String>,
    pub backlight_power: String,
    pub backlight_current: String,
}

/// Profiles shipped inside the binary, keyed by `hw.model`.
const BUNDLED: &[(&str, &str)] = &[("Mac15,11", include_str!("../profiles/Mac15,11.profile"))];

impl Profile {
    /// Loads the profile for a machine model: a `profiles/<model>.profile` file
    /// on disk if present (lets new models ship without a recompile), otherwise
    /// a profile bundled into the binary. `None` if the model is unmapped.
    pub fn for_model(model: &str) -> Option<Profile> {
        if let Ok(text) = fs::read_to_string(format!("profiles/{model}.profile")) {
            return Some(Profile::parse(&text));
        }
        BUNDLED
            .iter()
            .find(|(m, _)| *m == model)
            .map(|(_, text)| Profile::parse(text))
    }

    /// Models this binary ships a profile for.
    pub fn bundled_models() -> Vec<&'static str> {
        BUNDLED.iter().map(|(m, _)| *m).collect()
    }

    /// Parses the `field: key key key` profile format.
    fn parse(text: &str) -> Profile {
        let mut fields: HashMap<String, Vec<String>> = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                fields.insert(
                    key.trim().to_string(),
                    value.split_whitespace().map(|s| s.to_string()).collect(),
                );
            }
        }
        let list = |k: &str| fields.get(k).cloned().unwrap_or_default();
        let one = |k: &str| fields.get(k).and_then(|v| v.first().cloned()).unwrap_or_default();
        Profile {
            model: one("model"),
            cpu_p_temp: list("cpu_p_temp"),
            cpu_e_temp: list("cpu_e_temp"),
            gpu_temp: list("gpu_temp"),
            hotspot: list("hotspot"),
            dram_temp: list("dram_temp"),
            dram_power: one("dram_power"),
            ssd_temp: list("ssd_temp"),
            battery_temp: list("battery_temp"),
            fans: list("fans"),
            system_power: one("system_power"),
            cpu_clusters: list("cpu_clusters"),
            rails: list("rails"),
            adapter_voltage: list("adapter_voltage"),
            adapter_current: list("adapter_current"),
            backlight_power: one("backlight_power"),
            backlight_current: one("backlight_current"),
        }
    }
}
