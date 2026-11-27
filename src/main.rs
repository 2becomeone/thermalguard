#![windows_subsystem = "windows"]

mod config;
mod monitor;
mod popup;
mod sensors;
mod tray;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::ptr::null;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::CreateMutexW;

use crate::config::Config;
use crate::sensors::SensorSuite;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Tray,
    Help,
    Version,
    ConfigPath,
    Status,
    TestNotify,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut action = Action::Tray;
    let mut config_override: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => action = Action::Help,
            "--version" | "-V" => action = Action::Version,
            "--config-path" => action = Action::ConfigPath,
            "--once" | "--status" => action = Action::Status,
            "--test-notify" => action = Action::TestNotify,
            "--config" => {
                i += 1;
                if i >= args.len() {
                    attach_console_if_needed();
                    eprintln!("thermal-guard: --config requires a file path");
                    flush_stdio();
                    return ExitCode::from(2);
                }
                config_override = Some(PathBuf::from(&args[i]));
            }
            unknown => {
                attach_console_if_needed();
                eprintln!("thermal-guard: unknown argument '{unknown}' (try --help)");
                flush_stdio();
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    if action != Action::Tray {
        attach_console_if_needed();
    }

    let config_path = config_override.unwrap_or_else(Config::default_config_path);

    let exit_code = match action {
        Action::Help => {
            print_help();
            ExitCode::SUCCESS
        }
        Action::Version => {
            println!("thermal-guard {VERSION}");
            ExitCode::SUCCESS
        }
        Action::ConfigPath => {
            println!("{}", config_path.display());
            ExitCode::SUCCESS
        }
        Action::Status => {
            run_status(&config_path);
            ExitCode::SUCCESS
        }
        Action::TestNotify => {
            let (cfg, _) = Config::load_or_create(&config_path);
            match tray::run_test_notification(cfg.notification_mode, cfg.sound) {
                Ok(via) => {
                    println!("Test notification sent via {via}.");
                    ExitCode::SUCCESS
                }
                Err(code) => {
                    eprintln!("Failed to send test notification (Win32 error {code}).");
                    ExitCode::from(1)
                }
            }
        }
        Action::Tray => {
            let mutex_name: Vec<u16> = "Local\\ThermalGuard_SingleInstance"
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mutex = unsafe { CreateMutexW(null(), 0, mutex_name.as_ptr()) };
            if !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                unsafe {
                    let _ = CloseHandle(mutex);
                }
                return ExitCode::SUCCESS;
            }

            let (cfg, warn) = Config::load_or_create(&config_path);
            let res = tray::run_tray_loop(config_path, cfg, warn);

            if !mutex.is_null() {
                unsafe {
                    let _ = CloseHandle(mutex);
                }
            }

            match res {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::from(1),
            }
        }
    };

    if action != Action::Tray {
        flush_stdio();
    }
    exit_code
}

fn run_status(config_path: &PathBuf) {
    let (cfg, warn) = Config::load_or_create(config_path);
    let mut suite = SensorSuite::new();
    suite.prime_pdh();
    thread::sleep(Duration::from_millis(250));
    let snap = suite.poll();

    println!("Thermal Guard v{VERSION}");
    println!(
        "Privilege : {}",
        if snap.is_elevated {
            "Administrator (elevated)"
        } else {
            "Standard user"
        }
    );
    println!("Config    : {}", config_path.display());
    if let Some(w) = warn {
        println!("Cfg Warn  : {w}");
    }

    println!();
    println!("[CPU]");
    println!("  Backend : {}", snap.cpu_backend);
    if let Some(cpu) = &snap.cpu {
        println!("  Device  : {}", cpu.name);
        println!("  Temp    : {:.1}°C ({})", cpu.temp_c, cpu.source);
        if let Some(lim) = cpu.throttle_limit_c {
            let headroom = lim - cpu.temp_c;
            println!("  Limit   : {:.0}°C (headroom {:+.1}°C)", lim, headroom);
        }
        println!(
            "  Throttle: {}",
            if cpu.is_throttling {
                cpu.throttle_reason.as_deref().unwrap_or("ACTIVE")
            } else {
                "Not throttling"
            }
        );
    } else {
        println!("  Temp    : Unavailable");
        println!("  Hint    : {}", snap.cpu_status_hint);
    }
    if let Some((flags, pct)) = snap.cpu_perf_limit {
        println!(
            "  WinPDH  : PerfLimitFlags=0x{flags:X}, %PerfLimit={pct:.1}%"
        );
    }

    println!();
    println!("[GPU]");
    println!("  Backend : {}", snap.gpu_backend);
    if let Some(gpu) = &snap.gpu {
        println!("  Device  : {}", gpu.name);
        println!("  Temp    : {:.1}°C ({})", gpu.temp_c, gpu.source);
        if let Some(lim) = gpu.throttle_limit_c {
            let headroom = lim - gpu.temp_c;
            println!("  Limit   : {:.0}°C (headroom {:+.1}°C)", lim, headroom);
        }
        println!(
            "  Throttle: {}",
            if gpu.is_throttling {
                gpu.throttle_reason.as_deref().unwrap_or("ACTIVE")
            } else {
                "Not throttling"
            }
        );
        if let Some(fan) = gpu.fan_percent {
            println!("  Fan     : {fan}%");
        }
    } else {
        println!("  Temp    : Unavailable");
        println!("  Hint    : {}", snap.gpu_status_hint);
    }

    println!();
    println!("[Thresholds]");
    println!(
        "  Poll    : {} ms | Hysteresis: {:.1}°C | Warn sustain: {}s",
        cfg.poll_interval_ms, cfg.hysteresis_c, cfg.warn_sustain_secs
    );
    println!(
        "  Notify  : {} (popup {}s)",
        cfg.notification_mode.as_str(),
        cfg.popup_duration_secs
    );
    println!(
        "  CPU     : {} | warn {:.0}°C | crit {:.0}°C | rapid +{:.0}°C/{}s (above {:.0}°C)",
        if cfg.cpu.enabled { "enabled" } else { "disabled" },
        cfg.cpu.warn_c,
        cfg.cpu.critical_c,
        cfg.cpu.rapid_rise_delta_c,
        cfg.cpu.rapid_rise_window_secs,
        cfg.cpu.rapid_rise_min_temp_c
    );
    println!(
        "  GPU     : {} | warn {:.0}°C | crit {:.0}°C | rapid +{:.0}°C/{}s (above {:.0}°C)",
        if cfg.gpu.enabled { "enabled" } else { "disabled" },
        cfg.gpu.warn_c,
        cfg.gpu.critical_c,
        cfg.gpu.rapid_rise_delta_c,
        cfg.gpu.rapid_rise_window_secs,
        cfg.gpu.rapid_rise_min_temp_c
    );
}

fn print_help() {
    println!(
        "thermal-guard {VERSION} - Lightweight Windows CPU & GPU thermal alert tray monitor\n\n\
         USAGE:\n\
           thermal-guard.exe [OPTIONS]\n\n\
         OPTIONS:\n\
           --status, --once     Print current CPU & GPU temperatures, backends, and thresholds\n\
           --test-notify        Send a test Windows tray notification and exit\n\
           --config <PATH>      Use a custom config.toml file path\n\
           --config-path        Print the active config.toml path and exit\n\
           -h, --help           Show this help message\n\
           -V, --version        Show version\n\n\
         With no action flag, runs quietly in the Windows system tray."
    );
}

fn attach_console_if_needed() {
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        if out.is_null() || out == INVALID_HANDLE_VALUE {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

fn flush_stdio() {
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
}
