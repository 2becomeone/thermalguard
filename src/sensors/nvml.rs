use std::ffi::{CStr, c_char, c_int, c_uint, c_ulonglong, c_void};
use std::mem::size_of;

use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::monitor::{SensorKind, SensorReading};

type NvmlDevice = *mut c_void;

const NVML_SUCCESS: c_int = 0;
const NVML_TEMPERATURE_GPU: c_uint = 0;
const NVML_TEMPERATURE_THRESHOLD_SLOWDOWN: c_uint = 1;
const NVML_TEMPERATURE_THRESHOLD_GPU_MAX: c_uint = 3;

const SW_THERMAL_SLOWDOWN: u64 = 0x0000_0000_0000_0020;
const HW_THERMAL_SLOWDOWN: u64 = 0x0000_0000_0000_0040;

#[repr(C)]
struct NvmlMarginTemperature {
    version: u32,
    margin_temperature: i32,
}

const NVML_MARGIN_TEMPERATURE_V1: u32 =
    (size_of::<NvmlMarginTemperature>() as u32) | (1u32 << 24);

type FnNvmlInitV2 = unsafe extern "C" fn() -> c_int;
type FnNvmlShutdown = unsafe extern "C" fn() -> c_int;
type FnNvmlDeviceGetCountV2 = unsafe extern "C" fn(*mut c_uint) -> c_int;
type FnNvmlDeviceGetHandleByIndexV2 = unsafe extern "C" fn(c_uint, *mut NvmlDevice) -> c_int;
type FnNvmlDeviceGetName = unsafe extern "C" fn(NvmlDevice, *mut c_char, c_uint) -> c_int;
type FnNvmlDeviceGetTemperature = unsafe extern "C" fn(NvmlDevice, c_uint, *mut c_uint) -> c_int;
type FnNvmlDeviceGetTemperatureThreshold =
    unsafe extern "C" fn(NvmlDevice, c_uint, *mut c_uint) -> c_int;
type FnNvmlDeviceGetMarginTemperature =
    unsafe extern "C" fn(NvmlDevice, *mut NvmlMarginTemperature) -> c_int;
type FnNvmlDeviceGetThrottleReasons =
    unsafe extern "C" fn(NvmlDevice, *mut c_ulonglong) -> c_int;
type FnNvmlDeviceGetFanSpeed = unsafe extern "C" fn(NvmlDevice, *mut c_uint) -> c_int;

/// Native NVIDIA GPU thermal reader backed directly by `nvml.dll`.
pub struct NvmlGpuSensor {
    module: HMODULE,
    shutdown: FnNvmlShutdown,
    get_count: FnNvmlDeviceGetCountV2,
    get_handle: FnNvmlDeviceGetHandleByIndexV2,
    get_name: FnNvmlDeviceGetName,
    get_temperature: FnNvmlDeviceGetTemperature,
    get_threshold: Option<FnNvmlDeviceGetTemperatureThreshold>,
    get_margin: Option<FnNvmlDeviceGetMarginTemperature>,
    get_event_reasons: Option<FnNvmlDeviceGetThrottleReasons>,
    get_throttle_reasons: Option<FnNvmlDeviceGetThrottleReasons>,
    get_fan_speed: Option<FnNvmlDeviceGetFanSpeed>,
}

impl NvmlGpuSensor {
    /// Attempts to load `nvml.dll`, initialize NVML, and verify at least one GPU is present.
    pub fn try_new() -> Result<Self, String> {
        let module = load_nvml_dll()?;

        unsafe {
            let init: FnNvmlInitV2 = load_sym(module, b"nvmlInit_v2\0")
                .ok_or_else(|| "missing nvmlInit_v2".to_string())?;
            let shutdown: FnNvmlShutdown = load_sym(module, b"nvmlShutdown\0")
                .ok_or_else(|| "missing nvmlShutdown".to_string())?;
            let get_count: FnNvmlDeviceGetCountV2 = load_sym(module, b"nvmlDeviceGetCount_v2\0")
                .ok_or_else(|| "missing nvmlDeviceGetCount_v2".to_string())?;
            let get_handle: FnNvmlDeviceGetHandleByIndexV2 =
                load_sym(module, b"nvmlDeviceGetHandleByIndex_v2\0")
                    .ok_or_else(|| "missing nvmlDeviceGetHandleByIndex_v2".to_string())?;
            let get_name: FnNvmlDeviceGetName = load_sym(module, b"nvmlDeviceGetName\0")
                .ok_or_else(|| "missing nvmlDeviceGetName".to_string())?;
            let get_temperature: FnNvmlDeviceGetTemperature =
                load_sym(module, b"nvmlDeviceGetTemperature\0")
                    .ok_or_else(|| "missing nvmlDeviceGetTemperature".to_string())?;

            if init() != NVML_SUCCESS {
                FreeLibrary(module);
                return Err("nvmlInit_v2 failed".to_string());
            }

            let mut count: c_uint = 0;
            if get_count(&mut count) != NVML_SUCCESS || count == 0 {
                let _ = shutdown();
                FreeLibrary(module);
                return Err("no NVIDIA GPU detected by NVML".to_string());
            }

            Ok(Self {
                module,
                shutdown,
                get_count,
                get_handle,
                get_name,
                get_temperature,
                get_threshold: load_sym(module, b"nvmlDeviceGetTemperatureThreshold\0"),
                get_margin: load_sym(module, b"nvmlDeviceGetMarginTemperature\0"),
                get_event_reasons: load_sym(module, b"nvmlDeviceGetCurrentClocksEventReasons\0"),
                get_throttle_reasons: load_sym(
                    module,
                    b"nvmlDeviceGetCurrentClocksThrottleReasons\0",
                ),
                get_fan_speed: load_sym(module, b"nvmlDeviceGetFanSpeed\0"),
            })
        }
    }

    /// Reads all NVIDIA GPUs and returns the hottest valid reading.
    pub fn read_hottest(&self) -> Option<SensorReading> {
        let mut count: c_uint = 0;
        if unsafe { (self.get_count)(&mut count) } != NVML_SUCCESS || count == 0 {
            return None;
        }

        let mut hottest: Option<SensorReading> = None;
        for idx in 0..count {
            if let Some(reading) = self.read_device(idx) {
                let replace = hottest
                    .as_ref()
                    .is_none_or(|prev| reading.temp_c > prev.temp_c);
                if replace {
                    hottest = Some(reading);
                }
            }
        }
        hottest
    }

    fn read_device(&self, index: c_uint) -> Option<SensorReading> {
        let mut dev: NvmlDevice = std::ptr::null_mut();
        if unsafe { (self.get_handle)(index, &mut dev) } != NVML_SUCCESS || dev.is_null() {
            return None;
        }

        let mut raw_temp: c_uint = 0;
        if unsafe { (self.get_temperature)(dev, NVML_TEMPERATURE_GPU, &mut raw_temp) } != NVML_SUCCESS
        {
            return None;
        }
        let temp_c = raw_temp as f32;
        if !(1.0..=130.0).contains(&temp_c) {
            return None;
        }

        let mut name_buf = [0 as c_char; 96];
        let name = if unsafe { (self.get_name)(dev, name_buf.as_mut_ptr(), 96) } == NVML_SUCCESS {
            unsafe { CStr::from_ptr(name_buf.as_ptr()) }
                .to_string_lossy()
                .trim()
                .to_string()
        } else {
            format!("NVIDIA GPU #{index}")
        };

        let throttle_limit_c = self.read_throttle_limit(dev, temp_c);
        let (is_throttling, throttle_reason) = self.read_thermal_throttle(dev);
        let fan_percent = self.read_fan_speed(dev);

        Some(SensorReading {
            kind: SensorKind::Gpu,
            name,
            source: "NVML".to_string(),
            temp_c,
            throttle_limit_c,
            is_throttling,
            throttle_reason,
            fan_percent,
        })
    }

    fn read_throttle_limit(&self, dev: NvmlDevice, temp_c: f32) -> Option<f32> {
        if let Some(get_margin) = self.get_margin {
            let mut margin = NvmlMarginTemperature {
                version: NVML_MARGIN_TEMPERATURE_V1,
                margin_temperature: 0,
            };
            if unsafe { get_margin(dev, &mut margin) } == NVML_SUCCESS
                && (-20..=120).contains(&margin.margin_temperature)
            {
                let limit = temp_c + margin.margin_temperature as f32;
                if (55.0..=125.0).contains(&limit) {
                    return Some(limit);
                }
            }
        }

        if let Some(get_threshold) = self.get_threshold {
            for kind in [
                NVML_TEMPERATURE_THRESHOLD_SLOWDOWN,
                NVML_TEMPERATURE_THRESHOLD_GPU_MAX,
            ] {
                let mut val: c_uint = 0;
                if unsafe { get_threshold(dev, kind, &mut val) } == NVML_SUCCESS
                    && (55..=125).contains(&val)
                {
                    return Some(val as f32);
                }
            }
        }

        None
    }

    fn read_thermal_throttle(&self, dev: NvmlDevice) -> (bool, Option<String>) {
        let mut reasons: u64 = 0;
        let ok = if let Some(f) = self.get_event_reasons {
            unsafe { f(dev, &mut reasons) == NVML_SUCCESS }
        } else if let Some(f) = self.get_throttle_reasons {
            unsafe { f(dev, &mut reasons) == NVML_SUCCESS }
        } else {
            false
        };

        if !ok {
            return (false, None);
        }

        let hw = (reasons & HW_THERMAL_SLOWDOWN) != 0;
        let sw = (reasons & SW_THERMAL_SLOWDOWN) != 0;
        match (hw, sw) {
            (true, true) => (true, Some("HW & SW Thermal Slowdown".to_string())),
            (true, false) => (true, Some("HW Thermal Slowdown".to_string())),
            (false, true) => (true, Some("SW Thermal Slowdown".to_string())),
            (false, false) => (false, None),
        }
    }

    fn read_fan_speed(&self, dev: NvmlDevice) -> Option<u32> {
        let get_fan = self.get_fan_speed?;
        let mut pct: c_uint = 0;
        if unsafe { get_fan(dev, &mut pct) } == NVML_SUCCESS && pct <= 100 {
            Some(pct)
        } else {
            None
        }
    }
}

impl Drop for NvmlGpuSensor {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.shutdown)();
            let _ = FreeLibrary(self.module);
        }
    }
}

fn load_nvml_dll() -> Result<HMODULE, String> {
    let candidates = [
        "nvml.dll",
        r"C:\Program Files\NVIDIA Corporation\NVSMI\nvml.dll",
    ];
    for path in candidates {
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let h = unsafe { LoadLibraryW(wide.as_ptr()) };
        if !h.is_null() {
            return Ok(h);
        }
    }
    Err("nvml.dll not found".to_string())
}

unsafe fn load_sym<T: Copy>(module: HMODULE, name_nul: &[u8]) -> Option<T> {
    debug_assert!(name_nul.ends_with(b"\0"));
    debug_assert_eq!(size_of::<T>(), size_of::<*const ()>());
    let proc_ptr = unsafe { GetProcAddress(module, name_nul.as_ptr()) }?;
    Some(unsafe { transmute_copy_fn(proc_ptr) })
}

#[inline]
unsafe fn transmute_copy_fn<Src: Copy, Dst: Copy>(src: Src) -> Dst {
    let ptr = &src as *const Src as *const Dst;
    unsafe { std::ptr::read(ptr) }
}
