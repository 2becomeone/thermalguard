use std::mem::size_of;
use std::ptr::{null, null_mut};

use windows_sys::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W,
    PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW,
    PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhGetFormattedCounterValue,
    PdhOpenQueryW,
};

use crate::monitor::{SensorKind, SensorReading};
use crate::sensors::cpu_brand_string;

const ERROR_SUCCESS_U32: u32 = 0;

/// Native Windows PDH reader for ACPI Thermal Zone temperatures.
/// Works without admin privileges on laptops/boards that expose ACPI thermal zones.
pub struct PdhAcpiThermal {
    query: PDH_HQUERY,
    high_prec_counter: Option<PDH_HCOUNTER>,
    std_counter: Option<PDH_HCOUNTER>,
}

impl PdhAcpiThermal {
    pub fn try_new() -> Option<Self> {
        let mut query: PDH_HQUERY = null_mut();
        if unsafe { PdhOpenQueryW(null(), 0, &mut query) } != ERROR_SUCCESS_U32 || query.is_null() {
            return None;
        }

        let high_prec_counter =
            add_english_counter(query, r"\Thermal Zone Information(*)\High Precision Temperature");
        let std_counter = add_english_counter(query, r"\Thermal Zone Information(*)\Temperature");

        if high_prec_counter.is_none() && std_counter.is_none() {
            unsafe { PdhCloseQuery(query) };
            return None;
        }

        let pdh = Self {
            query,
            high_prec_counter,
            std_counter,
        };

        // Prime and verify that at least one thermal zone instance exists.
        if pdh.read_hottest().is_none() {
            return None;
        }
        Some(pdh)
    }

    pub fn read_hottest(&self) -> Option<SensorReading> {
        if unsafe { PdhCollectQueryData(self.query) } != ERROR_SUCCESS_U32 {
            return None;
        }

        if let Some(counter) = self.high_prec_counter {
            if let Some(temp_c) = read_max_counter_array(counter, |v| (v / 10.0 - 273.15) as f32) {
                return Some(make_acpi_reading(temp_c));
            }
        }

        if let Some(counter) = self.std_counter {
            if let Some(temp_c) = read_max_counter_array(counter, |v| (v - 273.15) as f32) {
                return Some(make_acpi_reading(temp_c));
            }
        }

        None
    }
}

impl Drop for PdhAcpiThermal {
    fn drop(&mut self) {
        if !self.query.is_null() {
            unsafe {
                let _ = PdhCloseQuery(self.query);
            }
        }
    }
}

fn make_acpi_reading(temp_c: f32) -> SensorReading {
    SensorReading {
        kind: SensorKind::Cpu,
        name: cpu_brand_string(),
        source: "Windows ACPI Thermal Zone".to_string(),
        temp_c,
        throttle_limit_c: None,
        is_throttling: false,
        throttle_reason: None,
        fan_percent: None,
    }
}

/// Native Windows PDH CPU Performance Limit diagnostics reader.
pub struct PdhCpuThrottle {
    query: PDH_HQUERY,
    flags_counter: Option<PDH_HCOUNTER>,
    pct_counter: Option<PDH_HCOUNTER>,
}

impl PdhCpuThrottle {
    pub fn try_new() -> Option<Self> {
        let mut query: PDH_HQUERY = null_mut();
        if unsafe { PdhOpenQueryW(null(), 0, &mut query) } != ERROR_SUCCESS_U32 || query.is_null() {
            return None;
        }

        let flags_counter = add_english_counter(
            query,
            r"\Processor Information(_Total)\Performance Limit Flags",
        );
        let pct_counter =
            add_english_counter(query, r"\Processor Information(_Total)\% Performance Limit");

        if flags_counter.is_none() && pct_counter.is_none() {
            unsafe { PdhCloseQuery(query) };
            return None;
        }

        let pdh = Self {
            query,
            flags_counter,
            pct_counter,
        };
        let _ = unsafe { PdhCollectQueryData(pdh.query) };
        Some(pdh)
    }

    pub fn sample(&self) -> Option<(i64, f64)> {
        if unsafe { PdhCollectQueryData(self.query) } != ERROR_SUCCESS_U32 {
            return None;
        }

        let flags = self
            .flags_counter
            .and_then(read_large_value)
            .unwrap_or(0);
        let pct = self
            .pct_counter
            .and_then(read_double_value)
            .unwrap_or(100.0);

        Some((flags, pct))
    }
}

impl Drop for PdhCpuThrottle {
    fn drop(&mut self) {
        if !self.query.is_null() {
            unsafe {
                let _ = PdhCloseQuery(self.query);
            }
        }
    }
}

fn add_english_counter(query: PDH_HQUERY, path: &str) -> Option<PDH_HCOUNTER> {
    let path_w: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut counter: PDH_HCOUNTER = null_mut();
    let status = unsafe { PdhAddEnglishCounterW(query, path_w.as_ptr(), 0, &mut counter) };
    if status == ERROR_SUCCESS_U32 && !counter.is_null() {
        Some(counter)
    } else {
        None
    }
}

fn read_large_value(counter: PDH_HCOUNTER) -> Option<i64> {
    let mut val = PDH_FMT_COUNTERVALUE::default();
    let status = unsafe { PdhGetFormattedCounterValue(counter, PDH_FMT_LARGE, null_mut(), &mut val) };
    if status == ERROR_SUCCESS_U32
        && (val.CStatus == PDH_CSTATUS_VALID_DATA || val.CStatus == PDH_CSTATUS_NEW_DATA)
    {
        Some(unsafe { val.Anonymous.largeValue })
    } else {
        None
    }
}

fn read_double_value(counter: PDH_HCOUNTER) -> Option<f64> {
    let mut val = PDH_FMT_COUNTERVALUE::default();
    let status =
        unsafe { PdhGetFormattedCounterValue(counter, PDH_FMT_DOUBLE, null_mut(), &mut val) };
    if status == ERROR_SUCCESS_U32
        && (val.CStatus == PDH_CSTATUS_VALID_DATA || val.CStatus == PDH_CSTATUS_NEW_DATA)
    {
        let d = unsafe { val.Anonymous.doubleValue };
        if d.is_finite() { Some(d) } else { None }
    } else {
        None
    }
}

fn read_max_counter_array(counter: PDH_HCOUNTER, convert_c: fn(f64) -> f32) -> Option<f32> {
    let mut buf_size: u32 = 0;
    let mut item_count: u32 = 0;
    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut buf_size,
            &mut item_count,
            null_mut(),
        )
    };
    if status != PDH_MORE_DATA || buf_size == 0 {
        return None;
    }

    let item_size = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    let len = (buf_size as usize).div_ceil(item_size);
    let mut items = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); len];

    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut buf_size,
            &mut item_count,
            items.as_mut_ptr(),
        )
    };
    if status != ERROR_SUCCESS_U32 || item_count == 0 {
        return None;
    }

    let mut max_temp: Option<f32> = None;
    for item in items.into_iter().take(item_count as usize) {
        let cstatus = item.FmtValue.CStatus;
        if cstatus != PDH_CSTATUS_VALID_DATA && cstatus != PDH_CSTATUS_NEW_DATA {
            continue;
        }
        let raw = unsafe { item.FmtValue.Anonymous.doubleValue };
        if !raw.is_finite() {
            continue;
        }
        let temp_c = convert_c(raw);
        if (5.0..=130.0).contains(&temp_c) {
            max_temp = Some(max_temp.map_or(temp_c, |prev| prev.max(temp_c)));
        }
    }

    max_temp
}
