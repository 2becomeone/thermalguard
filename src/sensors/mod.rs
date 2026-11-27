pub mod coretemp;
pub mod hwinfo;
pub mod nvml;
pub mod pawnio;
pub mod pdh;

use std::mem::size_of;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::monitor::SensorReading;
use crate::sensors::nvml::NvmlGpuSensor;
use crate::sensors::pawnio::{PawnIoCpuSensor, PawnIoUnavailableReason};
use crate::sensors::pdh::{PdhAcpiThermal, PdhCpuThrottle};

const REPROBE_INTERVAL: Duration = Duration::from_secs(10);

enum CpuBackend {
    PawnIo(PawnIoCpuSensor),
    Hwinfo,
    CoreTemp,
    PdhAcpi(PdhAcpiThermal),
    None,
}

impl CpuBackend {
    fn label(&self) -> &'static str {
        match self {
            CpuBackend::PawnIo(_) => "PawnIO MSR",
            CpuBackend::Hwinfo => "HWiNFO64 Shared Memory",
            CpuBackend::CoreTemp => "Core Temp Shared Memory",
            CpuBackend::PdhAcpi(_) => "Windows ACPI Thermal Zone",
            CpuBackend::None => "Unavailable",
        }
    }
}

/// Result of a single hardware polling pass across CPU and GPU sensors.
#[derive(Debug, Clone)]
pub struct PollSnapshot {
    pub cpu: Option<SensorReading>,
    pub gpu: Option<SensorReading>,
    pub cpu_backend: String,
    pub gpu_backend: String,
    pub cpu_status_hint: String,
    pub gpu_status_hint: String,
    pub cpu_perf_limit: Option<(i64, f64)>,
    pub is_elevated: bool,
}

/// Auto-detecting multi-backend sensor manager for CPU and GPU telemetry.
pub struct SensorSuite {
    nvml: Option<NvmlGpuSensor>,
    nvml_err: Option<String>,
    cpu_backend: CpuBackend,
    cpu_hint: String,
    cpu_missed_polls: u32,
    last_cpu_probe: Instant,
    pdh_throttle: Option<PdhCpuThrottle>,
    is_elevated: bool,
}

impl SensorSuite {
    pub fn new() -> Self {
        let is_elevated = is_process_elevated();
        let (nvml, nvml_err) = match NvmlGpuSensor::try_new() {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(e)),
        };

        let mut suite = Self {
            nvml,
            nvml_err,
            cpu_backend: CpuBackend::None,
            cpu_hint: String::new(),
            cpu_missed_polls: 0,
            last_cpu_probe: Instant::now() - REPROBE_INTERVAL,
            pdh_throttle: PdhCpuThrottle::try_new(),
            is_elevated,
        };
        suite.probe_cpu_backend();
        suite
    }

    pub fn prime_pdh(&self) {
        if let Some(pdh) = &self.pdh_throttle {
            let _ = pdh.sample();
        }
        if let CpuBackend::PdhAcpi(acpi) = &self.cpu_backend {
            let _ = acpi.read_hottest();
        }
    }

    pub fn poll(&mut self) -> PollSnapshot {
        let need_reprobe = matches!(self.cpu_backend, CpuBackend::None) || self.cpu_missed_polls >= 3;
        if need_reprobe && self.last_cpu_probe.elapsed() >= REPROBE_INTERVAL {
            self.probe_cpu_backend();
        }

        // Read HWiNFO at most once per poll if needed by either CPU or GPU fallback.
        let need_hwinfo = matches!(self.cpu_backend, CpuBackend::Hwinfo) || self.nvml.is_none();
        let hwinfo_snapshot = if need_hwinfo {
            hwinfo::read_sensors()
        } else {
            None
        };

        let cpu = match &self.cpu_backend {
            CpuBackend::PawnIo(sensor) => sensor.read(),
            CpuBackend::Hwinfo => hwinfo_snapshot.as_ref().and_then(|s| s.cpu.clone()),
            CpuBackend::CoreTemp => coretemp::read_cpu(),
            CpuBackend::PdhAcpi(acpi) => acpi.read_hottest(),
            CpuBackend::None => None,
        };

        if matches!(self.cpu_backend, CpuBackend::None) {
            self.cpu_missed_polls = 0;
        } else if cpu.is_none() {
            self.cpu_missed_polls = self.cpu_missed_polls.saturating_add(1);
        } else {
            self.cpu_missed_polls = 0;
        }

        let (gpu, gpu_backend, gpu_status_hint) = if let Some(nvml) = &self.nvml {
            match nvml.read_hottest() {
                Some(reading) => (Some(reading), "NVML".to_string(), "Active".to_string()),
                None => (
                    None,
                    "NVML".to_string(),
                    "NVML loaded, but GPU temperature query returned no data".to_string(),
                ),
            }
        } else if let Some(hw_gpu) = hwinfo_snapshot.and_then(|s| s.gpu) {
            (
                Some(hw_gpu),
                "HWiNFO64 Shared Memory".to_string(),
                "Active (via HWiNFO64)".to_string(),
            )
        } else {
            (
                None,
                "Unavailable".to_string(),
                self.nvml_err
                    .clone()
                    .unwrap_or_else(|| "No NVIDIA GPU or HWiNFO64 GPU sensor detected".to_string()),
            )
        };

        let cpu_perf_limit = self.pdh_throttle.as_ref().and_then(PdhCpuThrottle::sample);

        PollSnapshot {
            cpu,
            gpu,
            cpu_backend: self.cpu_backend.label().to_string(),
            gpu_backend,
            cpu_status_hint: self.cpu_hint.clone(),
            gpu_status_hint,
            cpu_perf_limit,
            is_elevated: self.is_elevated,
        }
    }

    fn probe_cpu_backend(&mut self) {
        self.last_cpu_probe = Instant::now();
        self.cpu_missed_polls = 0;

        let pawn_reason = match PawnIoCpuSensor::try_new() {
            Ok(sensor) => {
                self.cpu_backend = CpuBackend::PawnIo(sensor);
                self.cpu_hint = "Active (Intel MSR via PawnIO)".to_string();
                return;
            }
            Err(reason) => Some(reason),
        };

        if hwinfo::read_sensors().is_some_and(|s| s.cpu.is_some()) {
            self.cpu_backend = CpuBackend::Hwinfo;
            self.cpu_hint = "Active (via HWiNFO64 Shared Memory)".to_string();
            return;
        }

        if coretemp::is_available() {
            self.cpu_backend = CpuBackend::CoreTemp;
            self.cpu_hint = "Active (via Core Temp Shared Memory)".to_string();
            return;
        }

        if let Some(acpi) = PdhAcpiThermal::try_new() {
            self.cpu_backend = CpuBackend::PdhAcpi(acpi);
            self.cpu_hint = "Active (Windows ACPI Thermal Zone)".to_string();
            return;
        }

        self.cpu_backend = CpuBackend::None;
        self.cpu_hint = match pawn_reason {
            Some(PawnIoUnavailableReason::NotInstalled) => {
                "Install PawnIO (winget install -e --id namazso.PawnIO) and run as Admin, or run HWiNFO64 / Core Temp".to_string()
            }
            Some(PawnIoUnavailableReason::AccessDenied) => {
                "Run as Administrator to read CPU temp via PawnIO (or run HWiNFO64 / Core Temp)".to_string()
            }
            Some(PawnIoUnavailableReason::Unsupported(reason)) => {
                format!("{reason} — run HWiNFO64 (Shared Memory) or Core Temp for CPU temperature")
            }
            None => {
                "Run HWiNFO64 (Shared Memory) or Core Temp for CPU temperature".to_string()
            }
        };
    }
}

/// Returns true if the current Windows process holds an elevated Administrator token.
pub fn is_process_elevated() -> bool {
    let mut token: HANDLE = null_mut();
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if opened == 0 || token.is_null() {
        return false;
    }

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned_len: u32 = 0;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned_len,
        )
    };
    unsafe { CloseHandle(token) };

    ok != 0 && elevation.TokenIsElevated != 0
}

/// Checks CPUID leaf 0 vendor string for `"GenuineIntel"`.
pub fn is_intel_cpu() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let res = std::arch::x86_64::__cpuid(0);
        let mut vendor = [0u8; 12];
        vendor[0..4].copy_from_slice(&res.ebx.to_le_bytes());
        vendor[4..8].copy_from_slice(&res.edx.to_le_bytes());
        vendor[8..12].copy_from_slice(&res.ecx.to_le_bytes());
        &vendor == b"GenuineIntel"
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Reads the processor brand string from CPUID extended leaves `0x80000002..=0x80000004`.
pub fn cpu_brand_string() -> String {
    #[cfg(target_arch = "x86_64")]
    {
        let max_ext = std::arch::x86_64::__cpuid(0x8000_0000).eax;
        if max_ext >= 0x8000_0004 {
            let mut raw = [0u8; 48];
            for (i, leaf) in [0x8000_0002_u32, 0x8000_0003, 0x8000_0004]
                .into_iter()
                .enumerate()
            {
                let res = std::arch::x86_64::__cpuid(leaf);
                let base = i * 16;
                raw[base..base + 4].copy_from_slice(&res.eax.to_le_bytes());
                raw[base + 4..base + 8].copy_from_slice(&res.ebx.to_le_bytes());
                raw[base + 8..base + 12].copy_from_slice(&res.ecx.to_le_bytes());
                raw[base + 12..base + 16].copy_from_slice(&res.edx.to_le_bytes());
            }
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            let s = String::from_utf8_lossy(&raw[..end]).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    "CPU".to_string()
}
