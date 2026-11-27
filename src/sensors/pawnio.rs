use cpu_temp::msr::intel_msr::IntelMsr;
use cpu_temp::pawnio::PawnIo;

use crate::monitor::{SensorKind, SensorReading};
use crate::sensors::{cpu_brand_string, is_intel_cpu, is_process_elevated};

#[derive(Debug, Clone)]
pub enum PawnIoUnavailableReason {
    NotInstalled,
    AccessDenied,
    Unsupported(String),
}

/// Direct Intel CPU package & core thermal sensor via PawnIO signed MSR driver.
pub struct PawnIoCpuSensor {
    msr: IntelMsr,
    cpu_name: String,
    tj_max: f32,
    throttle_limit_c: f32,
}

impl PawnIoCpuSensor {
    pub fn try_new() -> Result<Self, PawnIoUnavailableReason> {
        if !is_intel_cpu() {
            return Err(PawnIoUnavailableReason::Unsupported(
                "PawnIO IntelMSR requires an Intel CPU".to_string(),
            ));
        }

        let msr = match IntelMsr::new() {
            Ok(msr) => msr,
            Err(err) => {
                if !PawnIo::is_installed() {
                    return Err(PawnIoUnavailableReason::NotInstalled);
                }
                let msg = err.to_string();
                if !is_process_elevated()
                    || msg.contains("0x80070005")
                    || msg.to_ascii_lowercase().contains("access is denied")
                {
                    return Err(PawnIoUnavailableReason::AccessDenied);
                }
                return Err(PawnIoUnavailableReason::Unsupported(msg));
            }
        };

        let (tj_max, throttle_limit_c) = match msr.read_temperature_target() {
            Ok(target) => {
                let raw_tj = target.temperature_target() as f32;
                let tj = if (50.0..=125.0).contains(&raw_tj) {
                    raw_tj
                } else {
                    100.0
                };
                let raw_off = target.target_offset() as f32;
                let offset = if raw_off <= 40.0 { raw_off } else { 0.0 };
                (tj, (tj - offset).max(60.0))
            }
            Err(_) => (100.0, 100.0),
        };

        let sensor = Self {
            msr,
            cpu_name: cpu_brand_string(),
            tj_max,
            throttle_limit_c,
        };

        // Verify at least one thermal MSR reads valid data before claiming success.
        if sensor.read().is_none() {
            return Err(PawnIoUnavailableReason::Unsupported(
                "Intel thermal MSRs (0x1B1 / 0x19C) did not return a valid reading".to_string(),
            ));
        }

        Ok(sensor)
    }

    pub fn read(&self) -> Option<SensorReading> {
        if let Ok(pkg) = self.msr.read_package_therm_status() {
            let readout = pkg.pkg_digital_readout() as f32;
            let temp_c = self.tj_max - readout;
            if (1.0..=130.0).contains(&temp_c) {
                let (is_throttling, throttle_reason) = format_throttle_flags(
                    pkg.pkg_thermal_status(),
                    pkg.pkg_prochot_event(),
                    pkg.pkg_critical_temp_status(),
                );
                return Some(SensorReading {
                    kind: SensorKind::Cpu,
                    name: self.cpu_name.clone(),
                    source: "PawnIO MSR".to_string(),
                    temp_c,
                    throttle_limit_c: Some(self.throttle_limit_c),
                    is_throttling,
                    throttle_reason,
                    fan_percent: None,
                });
            }
        }

        if let Ok(core) = self.msr.read_therm_status() {
            if core.reading_valid() {
                let temp_c = self.tj_max - core.digital_readout() as f32;
                if (1.0..=130.0).contains(&temp_c) {
                    let (is_throttling, throttle_reason) = format_throttle_flags(
                        core.thermal_status(),
                        core.prochot_event(),
                        core.critical_temp_status(),
                    );
                    return Some(SensorReading {
                        kind: SensorKind::Cpu,
                        name: self.cpu_name.clone(),
                        source: "PawnIO MSR (Core)".to_string(),
                        temp_c,
                        throttle_limit_c: Some(self.throttle_limit_c),
                        is_throttling,
                        throttle_reason,
                        fan_percent: None,
                    });
                }
            }
        }

        None
    }
}

fn format_throttle_flags(
    thermal_status: bool,
    prochot_event: bool,
    critical_temp: bool,
) -> (bool, Option<String>) {
    let mut parts = Vec::new();
    if critical_temp {
        parts.push("Critical Temp");
    }
    if thermal_status {
        parts.push("TCC Thermal Throttle");
    }
    if prochot_event {
        parts.push("PROCHOT# Active");
    }
    if parts.is_empty() {
        (false, None)
    } else {
        (true, Some(parts.join(", ")))
    }
}
