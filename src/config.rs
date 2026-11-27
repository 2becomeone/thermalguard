use std::fs;
use std::path::{Path, PathBuf};

/// Controls how Thermal Guard delivers alert notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationMode {
    /// Try Windows tray balloon first, falling back to the built-in floating popup if unavailable.
    Auto,
    /// Always use the built-in non-focus-stealing floating toast popup.
    Popup,
    /// Only use Windows native tray balloons.
    Windows,
}

impl NotificationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            NotificationMode::Auto => "auto",
            NotificationMode::Popup => "popup",
            NotificationMode::Windows => "windows",
        }
    }
}

/// Per-sensor thermal thresholds and notification cooldowns.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorThresholds {
    pub enabled: bool,
    pub warn_c: f32,
    pub critical_c: f32,
    pub rapid_rise_delta_c: f32,
    pub rapid_rise_window_secs: u64,
    pub rapid_rise_min_temp_c: f32,
    pub warn_cooldown_secs: u64,
    pub critical_cooldown_secs: u64,
    pub rapid_rise_cooldown_secs: u64,
}

impl SensorThresholds {
    /// Default thresholds tuned for modern desktop CPUs (TjMax ~100°C).
    pub fn default_cpu() -> Self {
        Self {
            enabled: true,
            warn_c: 80.0,
            critical_c: 90.0,
            rapid_rise_delta_c: 12.0,
            rapid_rise_window_secs: 5,
            rapid_rise_min_temp_c: 65.0,
            warn_cooldown_secs: 90,
            critical_cooldown_secs: 30,
            rapid_rise_cooldown_secs: 45,
        }
    }

    /// Default thresholds tuned for modern discrete GPUs (slowdown ~83-88°C).
    pub fn default_gpu() -> Self {
        Self {
            enabled: true,
            warn_c: 76.0,
            critical_c: 83.0,
            rapid_rise_delta_c: 8.0,
            rapid_rise_window_secs: 5,
            rapid_rise_min_temp_c: 60.0,
            warn_cooldown_secs: 90,
            critical_cooldown_secs: 30,
            rapid_rise_cooldown_secs: 45,
        }
    }

    fn clamp(&mut self) {
        self.warn_c = clamp_finite(self.warn_c, 30.0, 125.0, 80.0);
        self.critical_c = clamp_finite(self.critical_c, self.warn_c + 1.0, 130.0, 90.0);
        self.rapid_rise_delta_c = clamp_finite(self.rapid_rise_delta_c, 2.0, 50.0, 10.0);
        self.rapid_rise_window_secs = self.rapid_rise_window_secs.clamp(2, 120);
        self.rapid_rise_min_temp_c = clamp_finite(self.rapid_rise_min_temp_c, 20.0, 120.0, 60.0);
        self.warn_cooldown_secs = self.warn_cooldown_secs.clamp(5, 3600);
        self.critical_cooldown_secs = self.critical_cooldown_secs.clamp(5, 3600);
        self.rapid_rise_cooldown_secs = self.rapid_rise_cooldown_secs.clamp(5, 3600);
    }
}

/// Application configuration loaded from `config.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub poll_interval_ms: u64,
    pub hysteresis_c: f32,
    pub warn_sustain_secs: u64,
    pub notify_cooldown: bool,
    pub sound: bool,
    pub beep_on_critical: bool,
    pub notification_mode: NotificationMode,
    pub popup_duration_secs: u64,
    pub cpu: SensorThresholds,
    pub gpu: SensorThresholds,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval_ms: 1000,
            hysteresis_c: 5.0,
            warn_sustain_secs: 3,
            notify_cooldown: true,
            sound: true,
            beep_on_critical: false,
            notification_mode: NotificationMode::Auto,
            popup_duration_secs: 7,
            cpu: SensorThresholds::default_cpu(),
            gpu: SensorThresholds::default_gpu(),
        }
    }
}

impl Config {
    /// Returns the default configuration file path: `%APPDATA%\thermal-guard\config.toml`,
    /// falling back next to the current executable.
    pub fn default_config_path() -> PathBuf {
        default_config_path()
    }

    /// Loads config from `path`, or writes a commented default file if it does not exist.
    /// Malformed lines produce a non-fatal warning string while preserving valid settings.
    pub fn load_or_create(path: &Path) -> (Config, Option<String>) {
        if !path.exists() {
            let cfg = Config::default();
            if let Some(parent) = path.parent() {
                if let Err(err) = fs::create_dir_all(parent) {
                    return (
                        cfg,
                        Some(format!(
                            "Could not create config directory '{}': {err}",
                            parent.display()
                        )),
                    );
                }
            }
            if let Err(err) = fs::write(path, cfg.to_toml_string()) {
                return (
                    cfg,
                    Some(format!(
                        "Could not write default config '{}': {err}",
                        path.display()
                    )),
                );
            }
            return (cfg, None);
        }

        match fs::read_to_string(path) {
            Ok(raw) => {
                let (mut cfg, warnings) = Self::parse_toml_lenient(&raw);
                cfg.clamp();
                let warning = if warnings.is_empty() {
                    None
                } else {
                    Some(warnings.join("; "))
                };
                (cfg, warning)
            }
            Err(err) => (
                Config::default(),
                Some(format!("Could not read config '{}': {err}", path.display())),
            ),
        }
    }

    /// Serializes the configuration into a clean, commented TOML string.
    pub fn to_toml_string(&self) -> String {
        let mut out = String::new();
        out.push_str("# Thermal Guard configuration\n");
        out.push_str("# Edit and click \"Reload Config\" in the tray menu to apply changes.\n\n");
        out.push_str(&format!("poll_interval_ms = {}\n", self.poll_interval_ms));
        out.push_str(&format!("hysteresis_c = {}\n", fmt_f32(self.hysteresis_c)));
        out.push_str(&format!("warn_sustain_secs = {}\n", self.warn_sustain_secs));
        out.push_str(&format!("notify_cooldown = {}\n", self.notify_cooldown));
        out.push_str(&format!("sound = {}\n", self.sound));
        out.push_str(&format!("beep_on_critical = {}\n", self.beep_on_critical));
        out.push_str(&format!(
            "notification_mode = \"{}\"\n",
            self.notification_mode.as_str()
        ));
        out.push_str(&format!(
            "popup_duration_secs = {}\n",
            self.popup_duration_secs
        ));

        write_sensor_section(&mut out, "cpu", &self.cpu);
        write_sensor_section(&mut out, "gpu", &self.gpu);
        out
    }

    /// Strictly parses a TOML string into `Config`, returning `Err` on any malformed line or value.
    #[cfg(test)]
    pub fn parse_toml(s: &str) -> Result<Config, String> {
        let (mut cfg, warnings) = Self::parse_toml_lenient(s);
        if !warnings.is_empty() {
            return Err(warnings.join("; "));
        }
        cfg.clamp();
        Ok(cfg)
    }

    fn clamp(&mut self) {
        self.poll_interval_ms = self.poll_interval_ms.clamp(250, 10_000);
        self.hysteresis_c = clamp_finite(self.hysteresis_c, 1.0, 20.0, 5.0);
        self.warn_sustain_secs = self.warn_sustain_secs.clamp(0, 120);
        self.popup_duration_secs = self.popup_duration_secs.clamp(2, 60);
        self.cpu.clamp();
        self.gpu.clamp();
    }

    fn parse_toml_lenient(s: &str) -> (Config, Vec<String>) {
        #[derive(Copy, Clone, PartialEq, Eq)]
        enum Section {
            Root,
            Cpu,
            Gpu,
            Unknown,
        }

        let mut cfg = Config::default();
        let mut warnings = Vec::new();
        let mut section = Section::Root;

        for (idx, raw_line) in s.lines().enumerate() {
            let line_no = idx + 1;
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }

            if line.starts_with('[') && line.ends_with(']') {
                let name = line[1..line.len() - 1].trim();
                section = match name {
                    "cpu" => Section::Cpu,
                    "gpu" => Section::Gpu,
                    _ => {
                        warnings.push(format!("line {line_no}: unknown section [{name}]"));
                        Section::Unknown
                    }
                };
                continue;
            }

            let Some((raw_key, raw_val)) = line.split_once('=') else {
                warnings.push(format!("line {line_no}: expected 'key = value'"));
                continue;
            };

            let key = raw_key.trim();
            let val = raw_val.trim();

            if key.is_empty() || val.is_empty() {
                warnings.push(format!("line {line_no}: empty key or value"));
                continue;
            }

            match section {
                Section::Root => match key {
                    "poll_interval_ms" => parse_into_u64(val, line_no, key, &mut cfg.poll_interval_ms, &mut warnings),
                    "hysteresis_c" => parse_into_f32(val, line_no, key, &mut cfg.hysteresis_c, &mut warnings),
                    "warn_sustain_secs" => parse_into_u64(val, line_no, key, &mut cfg.warn_sustain_secs, &mut warnings),
                    "notify_cooldown" => parse_into_bool(val, line_no, key, &mut cfg.notify_cooldown, &mut warnings),
                    "sound" => parse_into_bool(val, line_no, key, &mut cfg.sound, &mut warnings),
                    "beep_on_critical" => parse_into_bool(val, line_no, key, &mut cfg.beep_on_critical, &mut warnings),
                    "notification_mode" => parse_into_notification_mode(
                        val,
                        line_no,
                        key,
                        &mut cfg.notification_mode,
                        &mut warnings,
                    ),
                    "popup_duration_secs" => parse_into_u64(
                        val,
                        line_no,
                        key,
                        &mut cfg.popup_duration_secs,
                        &mut warnings,
                    ),
                    _ => warnings.push(format!("line {line_no}: unknown key '{key}'")),
                },
                Section::Cpu => apply_sensor_key(&mut cfg.cpu, key, val, line_no, &mut warnings),
                Section::Gpu => apply_sensor_key(&mut cfg.gpu, key, val, line_no, &mut warnings),
                Section::Unknown => {}
            }
        }

        cfg.clamp();
        (cfg, warnings)
    }
}

/// Returns `%APPDATA%\thermal-guard\config.toml`, or `<exe_dir>\config.toml` if `%APPDATA%` is unset.
pub fn default_config_path() -> PathBuf {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let mut p = PathBuf::from(appdata);
        p.push("thermal-guard");
        p.push("config.toml");
        return p;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("config.toml");
        }
    }
    PathBuf::from("config.toml")
}

fn write_sensor_section(out: &mut String, name: &str, t: &SensorThresholds) {
    out.push_str(&format!("\n[{name}]\n"));
    out.push_str(&format!("enabled = {}\n", t.enabled));
    out.push_str(&format!("warn_c = {}\n", fmt_f32(t.warn_c)));
    out.push_str(&format!("critical_c = {}\n", fmt_f32(t.critical_c)));
    out.push_str(&format!(
        "rapid_rise_delta_c = {}\n",
        fmt_f32(t.rapid_rise_delta_c)
    ));
    out.push_str(&format!(
        "rapid_rise_window_secs = {}\n",
        t.rapid_rise_window_secs
    ));
    out.push_str(&format!(
        "rapid_rise_min_temp_c = {}\n",
        fmt_f32(t.rapid_rise_min_temp_c)
    ));
    out.push_str(&format!("warn_cooldown_secs = {}\n", t.warn_cooldown_secs));
    out.push_str(&format!(
        "critical_cooldown_secs = {}\n",
        t.critical_cooldown_secs
    ));
    out.push_str(&format!(
        "rapid_rise_cooldown_secs = {}\n",
        t.rapid_rise_cooldown_secs
    ));
}

fn apply_sensor_key(
    target: &mut SensorThresholds,
    key: &str,
    val: &str,
    line_no: usize,
    warnings: &mut Vec<String>,
) {
    match key {
        "enabled" => parse_into_bool(val, line_no, key, &mut target.enabled, warnings),
        "warn_c" => parse_into_f32(val, line_no, key, &mut target.warn_c, warnings),
        "critical_c" => parse_into_f32(val, line_no, key, &mut target.critical_c, warnings),
        "rapid_rise_delta_c" => {
            parse_into_f32(val, line_no, key, &mut target.rapid_rise_delta_c, warnings)
        }
        "rapid_rise_window_secs" => {
            parse_into_u64(val, line_no, key, &mut target.rapid_rise_window_secs, warnings)
        }
        "rapid_rise_min_temp_c" => {
            parse_into_f32(val, line_no, key, &mut target.rapid_rise_min_temp_c, warnings)
        }
        "warn_cooldown_secs" => {
            parse_into_u64(val, line_no, key, &mut target.warn_cooldown_secs, warnings)
        }
        "critical_cooldown_secs" => {
            parse_into_u64(val, line_no, key, &mut target.critical_cooldown_secs, warnings)
        }
        "rapid_rise_cooldown_secs" => {
            parse_into_u64(val, line_no, key, &mut target.rapid_rise_cooldown_secs, warnings)
        }
        _ => warnings.push(format!("line {line_no}: unknown sensor key '{key}'")),
    }
}

fn parse_into_u64(
    val: &str,
    line_no: usize,
    key: &str,
    out: &mut u64,
    warnings: &mut Vec<String>,
) {
    let cleaned = val.replace('_', "");
    match cleaned.parse::<u64>() {
        Ok(v) => *out = v,
        Err(_) => warnings.push(format!("line {line_no}: invalid integer for '{key}': '{val}'")),
    }
}

fn parse_into_f32(
    val: &str,
    line_no: usize,
    key: &str,
    out: &mut f32,
    warnings: &mut Vec<String>,
) {
    let cleaned = val.replace('_', "");
    match cleaned.parse::<f32>() {
        Ok(v) if v.is_finite() => *out = v,
        _ => warnings.push(format!("line {line_no}: invalid float for '{key}': '{val}'")),
    }
}

fn parse_into_bool(
    val: &str,
    line_no: usize,
    key: &str,
    out: &mut bool,
    warnings: &mut Vec<String>,
) {
    match val {
        "true" => *out = true,
        "false" => *out = false,
        _ => warnings.push(format!("line {line_no}: invalid bool for '{key}': '{val}'")),
    }
}

fn parse_into_notification_mode(
    val: &str,
    line_no: usize,
    key: &str,
    out: &mut NotificationMode,
    warnings: &mut Vec<String>,
) {
    let trimmed = val.trim();
    let unquoted = if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    match unquoted.to_ascii_lowercase().as_str() {
        "auto" => *out = NotificationMode::Auto,
        "popup" => *out = NotificationMode::Popup,
        "windows" => *out = NotificationMode::Windows,
        _ => warnings.push(format!(
            "line {line_no}: invalid notification_mode for '{key}': '{val}' (expected auto, popup, or windows)"
        )),
    }
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(idx) => &line[..idx],
        None => line,
    }
}

fn fmt_f32(v: f32) -> String {
    if (v - v.round()).abs() < 1e-4 {
        format!("{:.1}", v)
    } else {
        format!("{v}")
    }
}

fn clamp_finite(v: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(min, max)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        let cfg = Config::default();
        let toml = cfg.to_toml_string();
        let parsed = Config::parse_toml(&toml).expect("round-trip should succeed");
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn custom_values_comments_and_clamping() {
        let input = r#"
# Top comment
poll_interval_ms = 50 # clamps to 250
hysteresis_c = 99.0   # clamps to 20.0
warn_sustain_secs = 4
notify_cooldown = false
sound = true
beep_on_critical = true
notification_mode = "Popup"
popup_duration_secs = 120 # clamps to 60

[cpu]
enabled = true
warn_c = 78.5
critical_c = 88.0
rapid_rise_delta_c = 10.0
rapid_rise_window_secs = 6
rapid_rise_min_temp_c = 62.0
warn_cooldown_secs = 60
critical_cooldown_secs = 20
rapid_rise_cooldown_secs = 30

[gpu]
enabled = false
warn_c = 74.0
critical_c = 81.0
"#;
        let parsed = Config::parse_toml(input).expect("valid custom config");
        assert_eq!(parsed.poll_interval_ms, 250);
        assert_eq!(parsed.hysteresis_c, 20.0);
        assert_eq!(parsed.warn_sustain_secs, 4);
        assert!(!parsed.notify_cooldown);
        assert!(parsed.sound);
        assert!(parsed.beep_on_critical);
        assert_eq!(parsed.notification_mode, NotificationMode::Popup);
        assert_eq!(parsed.popup_duration_secs, 60);

        assert!(parsed.cpu.enabled);
        assert_eq!(parsed.cpu.warn_c, 78.5);
        assert_eq!(parsed.cpu.critical_c, 88.0);
        assert_eq!(parsed.cpu.rapid_rise_delta_c, 10.0);
        assert_eq!(parsed.cpu.rapid_rise_window_secs, 6);
        assert_eq!(parsed.cpu.rapid_rise_min_temp_c, 62.0);
        assert_eq!(parsed.cpu.warn_cooldown_secs, 60);
        assert_eq!(parsed.cpu.critical_cooldown_secs, 20);
        assert_eq!(parsed.cpu.rapid_rise_cooldown_secs, 30);

        assert!(!parsed.gpu.enabled);
        assert_eq!(parsed.gpu.warn_c, 74.0);
        assert_eq!(parsed.gpu.critical_c, 81.0);
        assert_eq!(parsed.gpu.rapid_rise_delta_c, 8.0);
    }

    #[test]
    fn poll_interval_upper_clamp() {
        let parsed = Config::parse_toml(
            "poll_interval_ms = 99999\nhysteresis_c = 0.1\npopup_duration_secs = 0\nnotification_mode = windows",
        )
        .unwrap();
        assert_eq!(parsed.poll_interval_ms, 10_000);
        assert_eq!(parsed.hysteresis_c, 1.0);
        assert_eq!(parsed.popup_duration_secs, 2);
        assert_eq!(parsed.notification_mode, NotificationMode::Windows);
    }

    #[test]
    fn load_or_create_creates_file_and_reports_malformed_lines() {
        let dir = std::env::temp_dir().join(format!("thermal-guard-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");

        let (cfg, warn) = Config::load_or_create(&path);
        assert!(warn.is_none());
        assert_eq!(cfg, Config::default());
        assert!(path.exists());

        fs::write(
            &path,
            "poll_interval_ms = 500\nbad_line_without_equals\n[cpu]\nwarn_c = nope\ncritical_c = 92.0\n",
        )
        .unwrap();

        let (cfg2, warn2) = Config::load_or_create(&path);
        assert!(warn2.is_some());
        assert_eq!(cfg2.poll_interval_ms, 500);
        assert_eq!(cfg2.cpu.critical_c, 92.0);
        assert_eq!(cfg2.cpu.warn_c, 80.0);

        let _ = fs::remove_dir_all(&dir);
    }
}
