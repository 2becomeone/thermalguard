use std::mem::size_of;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Memory::{
    FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
    VirtualQuery,
};

use crate::monitor::{SensorKind, SensorReading};
use crate::sensors::cpu_brand_string;

const CORETEMP_NAMES: [&str; 2] = ["CoreTempMappingObjectEx", "CoreTempMappingObject"];
const CORETEMP_MIN_BYTES: usize = 2686;

/// Checks if Core Temp shared memory is currently available.
pub fn is_available() -> bool {
    read_cpu().is_some()
}

/// Reads the hottest CPU core temperature from Core Temp shared memory.
pub fn read_cpu() -> Option<SensorReading> {
    for name in CORETEMP_NAMES {
        if let Some(reading) = read_from_mapping(name) {
            return Some(reading);
        }
    }
    None
}

fn read_from_mapping(mapping_name: &str) -> Option<SensorReading> {
    let name_w: Vec<u16> = mapping_name.encode_utf16().chain(Some(0)).collect();
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

unsafe fn parse_mapped_region(base: *const u8) -> Option<SensorReading> {
    let mut mbi = MEMORY_BASIC_INFORMATION::default();
    let queried = unsafe { VirtualQuery(base as *const _, &mut mbi, size_of::<MEMORY_BASIC_INFORMATION>()) };
    if queried == 0 || mbi.RegionSize < CORETEMP_MIN_BYTES {
        return None;
    }

    let bytes = unsafe { std::slice::from_raw_parts(base, mbi.RegionSize) };

    // CoreTempSharedData layout (4-byte packed):
    // uiLoad[256]      : 0..1024
    // uiTjMax[128]     : 1024..1536
    // uiCoreCnt        : 1536..1540
    // uiCPUCnt         : 1540..1544
    // fTemp[256]       : 1544..2568
    // fVID..fMultiplier: 2568..2584
    // sCPUName[100]    : 2584..2684
    // ucFahrenheit     : 2684
    // ucDeltaToTjMax   : 2685
    let tj_max_raw = read_u32_le(bytes, 1024)?;
    let core_cnt = read_u32_le(bytes, 1536)? as usize;
    if core_cnt == 0 || core_cnt > 256 {
        return None;
    }

    let uc_fahrenheit = *bytes.get(2684)? != 0;
    let uc_delta_to_tjmax = *bytes.get(2685)? != 0;

    let tj_max = if (50..=125).contains(&tj_max_raw) {
        Some(tj_max_raw as f32)
    } else {
        None
    };

    let mut max_temp: Option<f32> = None;
    for i in 0..core_cnt {
        let val = read_f32_le(bytes, 1544 + i * 4)?;
        if !val.is_finite() {
            continue;
        }
        let mut temp_c = if uc_fahrenheit {
            (val - 32.0) * (5.0 / 9.0)
        } else {
            val
        };
        if uc_delta_to_tjmax {
            let tj = tj_max?;
            temp_c = tj - temp_c;
        }
        if (1.0..=135.0).contains(&temp_c) {
            max_temp = Some(max_temp.map_or(temp_c, |prev| prev.max(temp_c)));
        }
    }

    let temp_c = max_temp?;
    let name_raw = read_c_str(&bytes[2584..2684]);
    let name = if name_raw.is_empty() {
        cpu_brand_string()
    } else {
        name_raw
    };

    Some(SensorReading {
        kind: SensorKind::Cpu,
        name,
        source: "Core Temp".to_string(),
        temp_c,
        throttle_limit_c: tj_max,
        is_throttling: false,
        throttle_reason: None,
        fan_percent: None,
    })
}

fn read_u32_le(buf: &[u8], offset: usize) -> Option<u32> {
    let slice: [u8; 4] = buf.get(offset..offset + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(slice))
}

fn read_f32_le(buf: &[u8], offset: usize) -> Option<f32> {
    let slice: [u8; 4] = buf.get(offset..offset + 4)?.try_into().ok()?;
    Some(f32::from_le_bytes(slice))
}

fn read_c_str(buf: &[u8]) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_string()
}
