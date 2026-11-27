use std::mem::size_of;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Memory::{
    FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
    VirtualQuery,
};

use crate::monitor::{SensorKind, SensorReading};
use crate::sensors::cpu_brand_string;

const HWINFO_SHMEM_NAME: &str = "Global\\HWiNFO_SENS_SM2";
const HWINFO_HEADER_MIN_BYTES: usize = 44;
const HWINFO_READING_MIN_BYTES: usize = 316;
const HWINFO_SENSOR_TYPE_TEMP: u32 = 1;

#[derive(Debug, Default)]
pub struct HwinfoReadings {
    pub cpu: Option<SensorReading>,
    pub gpu: Option<SensorReading>,
}

/// Reads the best CPU and GPU temperature readings from HWiNFO64 shared memory (`Global\HWiNFO_SENS_SM2`).
pub fn read_sensors() -> Option<HwinfoReadings> {
    let name_w: Vec<u16> = HWINFO_SHMEM_NAME.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, name_w.as_ptr()) };
    if handle.is_null() {
        return None;
    }

    let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0) };
    if view.Value.is_null() {
        unsafe { CloseHandle(handle) };
        return None;
    }

    let result = unsafe { parse_mapped_region(view.Value as *const u8) };

    unsafe {
        let _ = UnmapViewOfFile(view);
        let _ = CloseHandle(handle);
    }

    result
}

unsafe fn parse_mapped_region(base: *const u8) -> Option<HwinfoReadings> {
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    let queried = unsafe { VirtualQuery(base as *const _, &mut mbi, size_of::<MEMORY_BASIC_INFORMATION>()) };
    if queried == 0 || mbi.RegionSize < HWINFO_HEADER_MIN_BYTES {
        return None;
    }

    let bytes = unsafe { std::slice::from_raw_parts(base, mbi.RegionSize) };
    let sig = &bytes[0..4];
    // Reject 'DEAD' and validate 'HWiS' (either ASCII order or MSVC little-endian multi-char 'SiWH').
    if sig != b"HWiS" && sig != b"SiWH" {
        return None;
    }

    let offset_reading = read_u32_le(bytes, 32)? as usize;
    let size_reading = read_u32_le(bytes, 36)? as usize;
    let num_reading = read_u32_le(bytes, 40)? as usize;

    if size_reading < HWINFO_READING_MIN_BYTES || num_reading == 0 {
        return None;
    }
    let total_bytes = size_reading.checked_mul(num_reading)?;
    let end = offset_reading.checked_add(total_bytes)?;
    if end > bytes.len() {
        return None;
    }

    let mut best_cpu: Option<(u32, String, f32)> = None;
    let mut best_gpu: Option<(u32, String, f32)> = None;

    for idx in 0..num_reading {
        let start = offset_reading + idx * size_reading;
        let elem = &bytes[start..start + size_reading];

        let reading_type = read_u32_le(elem, 0)?;
        if reading_type != HWINFO_SENSOR_TYPE_TEMP {
            continue;
        }

        let label_orig = read_c_str(&elem[12..140]);
        let label_user = read_c_str(&elem[140..268]);
        let unit = read_c_str(&elem[268..284]);
        let raw_val = read_f64_le(elem, 284)?;
        if !raw_val.is_finite() {
            continue;
        }

        let mut temp_c = raw_val as f32;
        if unit.contains('F') && !unit.contains('C') {
            temp_c = (temp_c - 32.0) * (5.0 / 9.0);
        }
        if !(1.0..=140.0).contains(&temp_c) {
            continue;
        }

        let label = if !label_user.is_empty() {
            label_user
        } else {
            label_orig
        };
        let lower = label.to_ascii_lowercase();

        if let Some(prio) = cpu_label_priority(&lower) {
            if best_cpu
                .as_ref()
                .is_none_or(|(p, _, t)| prio > *p || (prio == *p && temp_c > *t))
            {
                best_cpu = Some((prio, label.clone(), temp_c));
            }
        }

        if let Some(prio) = gpu_label_priority(&lower) {
            if best_gpu
                .as_ref()
                .is_none_or(|(p, _, t)| prio > *p || (prio == *p && temp_c > *t))
            {
                best_gpu = Some((prio, label, temp_c));
            }
        }
    }

    let cpu = best_cpu.map(|(_, label, temp_c)| SensorReading {
        kind: SensorKind::Cpu,
        name: cpu_brand_string(),
        source: format!("HWiNFO64 ({label})"),
        temp_c,
        throttle_limit_c: None,
        is_throttling: false,
        throttle_reason: None,
        fan_percent: None,
    });

    let gpu = best_gpu.map(|(_, label, temp_c)| SensorReading {
        kind: SensorKind::Gpu,
        name: "GPU".to_string(),
        source: format!("HWiNFO64 ({label})"),
        temp_c,
        throttle_limit_c: None,
        is_throttling: false,
        throttle_reason: None,
        fan_percent: None,
    });

    if cpu.is_none() && gpu.is_none() {
        None
    } else {
        Some(HwinfoReadings { cpu, gpu })
    }
}

fn cpu_label_priority(lower: &str) -> Option<u32> {
    if lower.contains("distance")
        || lower.contains("tjmax")
        || lower.contains("limit")
        || lower.contains("threshold")
        || lower.contains("offset")
        || lower.contains("gpu")
    {
        return None;
    }

    if lower.contains("cpu package") {
        Some(100)
    } else if lower.contains("core max")
        || lower.contains("cpu (tctl/tdie)")
        || lower.contains("tctl/tdie")
    {
        Some(90)
    } else if lower.contains("cpu die") {
        Some(80)
    } else if lower.contains("core temperatures") || lower.contains("cpu tdie") {
        Some(70)
    } else if lower.contains("cpu")
        && !lower.contains("fan")
        && !lower.contains("power")
        && !lower.contains("voltage")
    {
        Some(50)
    } else if lower.contains("p-core") || lower.contains("e-core") || lower.contains("core #") {
        Some(30)
    } else {
        None
    }
}

fn gpu_label_priority(lower: &str) -> Option<u32> {
    if lower.contains("distance") || lower.contains("limit") || lower.contains("threshold") {
        return None;
    }
    if lower == "gpu temperature" || lower.contains("gpu temperature") {
        Some(100)
    } else if lower.contains("gpu thermal diode") {
        Some(90)
    } else if lower.contains("gpu core") {
        Some(80)
    } else if lower.contains("gpu hot spot") || lower.contains("gpu hotspot") {
        Some(60)
    } else {
        None
    }
}

fn read_u32_le(buf: &[u8], offset: usize) -> Option<u32> {
    let slice: [u8; 4] = buf.get(offset..offset + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(slice))
}

fn read_f64_le(buf: &[u8], offset: usize) -> Option<f64> {
    let slice: [u8; 8] = buf.get(offset..offset + 8)?.try_into().ok()?;
    Some(f64::from_le_bytes(slice))
}

fn read_c_str(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_string()
}
