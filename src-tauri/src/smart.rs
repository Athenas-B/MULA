//! SMART data through smartmontools' `smartctl`: finding (or, with the user's consent,
//! installing) it, mapping a Windows drive to a smartctl device, and reading reports and
//! self-test status from its JSON output (`--json=o` also carries the usual text output).

use crate::elevated_helper;
use crate::error::CommandError;
use crate::util;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};

// ── Finding / installing smartctl ───────────────────────────────────────────

static SMARTCTL_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

fn find_smartctl() -> Option<PathBuf> {
    [
        r"C:\Program Files\smartmontools\bin\smartctl.exe",
        r"C:\Program Files (x86)\smartmontools\bin\smartctl.exe",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
    .or_else(|| find_on_path("smartctl"))
}

/// Finds `name` on PATH: the absolute path `where` reports (the elevated helper requires
/// one), else the bare name if it can be run.
fn find_on_path(name: &str) -> Option<PathBuf> {
    if let Ok(output) = Command::new("where").arg(name).output() {
        let out = String::from_utf8_lossy(&output.stdout);
        if let Some(first) = out.lines().map(str::trim).find(|l| !l.is_empty()) {
            let path = PathBuf::from(first);
            if path.is_absolute() && path.exists() {
                return Some(path);
            }
        }
    }
    Command::new(name).arg("--version").output().is_ok().then(|| PathBuf::from(name))
}

/// Finds smartctl. Never installs anything by itself; only successes are cached, so a
/// smartctl installed later is picked up without restarting MULA.
fn ensure_smartctl() -> Result<PathBuf, CommandError> {
    let mut guard = SMARTCTL_PATH.lock().map_err(|e| e.to_string())?;
    if let Some(path) = guard.as_ref() {
        return Ok(path.clone());
    }
    let path = find_smartctl().ok_or(CommandError::SmartctlMissing)?;
    *guard = Some(path.clone());
    Ok(path)
}

fn install_smartctl() -> Result<PathBuf, String> {
    if cfg!(not(target_os = "windows")) {
        return Err("smartctl not found. Please install smartmontools using your package manager.".to_string());
    }

    let winget = find_on_path("winget")
        .ok_or("winget not found. Please install smartmontools manually from https://www.smartmontools.org/")?;
    let output = Command::new(&winget)
        .args([
            "install",
            "--id",
            "smartmontools.smartmontools",
            "-e",
            "--accept-source-agreements",
            "--accept-package-agreements",
            "--disable-interactivity",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run winget: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "winget install failed.\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    find_smartctl().ok_or_else(|| {
        "smartmontools was installed but smartctl could not be found. Restart MULA or add \
         C:\\Program Files\\smartmontools\\bin to PATH."
            .to_string()
    })
}

/// Installs smartmontools with winget. Only called after the user agreed in the UI.
#[tauri::command]
pub async fn install_smartmontools() -> Result<String, CommandError> {
    util::logged_blocking("install_smartmontools", || {
        let path = install_smartctl()?;
        if let Ok(mut guard) = SMARTCTL_PATH.lock() {
            *guard = Some(path.clone());
        }
        Ok(path.to_string_lossy().to_string())
    })
    .await
}

// ── Mapping a drive to a smartctl device ────────────────────────────────────

/// Drive id → (smartctl device, device type). Resolving a drive enumerates every disk through
/// PowerShell and probes each smartctl device, which is far too slow to repeat on every poll.
static SMART_DEVICES: LazyLock<Mutex<HashMap<String, (String, String)>>> = LazyLock::new(Default::default);

fn cached_device(id: &str) -> Option<(String, String)> {
    SMART_DEVICES.lock().ok()?.get(id).cloned()
}

fn remember_device(id: &str, device: &str, dtype: &str) {
    if let Ok(mut map) = SMART_DEVICES.lock() {
        map.insert(id.to_string(), (device.to_string(), dtype.to_string()));
    }
}

fn forget_device(id: &str) {
    if let Ok(mut map) = SMART_DEVICES.lock() {
        map.remove(id);
    }
}

fn extract_physical_drive_number(id: &str) -> Result<u32, String> {
    let upper = id.to_uppercase();
    let prefix = r"\\.\PHYSICALDRIVE";
    let num_str = if upper.starts_with(prefix) {
        &id[prefix.len()..]
    } else if upper.starts_with("PHYSICALDRIVE") {
        &id["PHYSICALDRIVE".len()..]
    } else {
        return Err(format!("Unrecognized drive device ID: {id}"));
    };
    num_str.trim().parse::<u32>().map_err(|_| format!("Could not parse drive number from: {id}"))
}

fn sd_letter_to_number(s: &str) -> u32 {
    s.chars().fold(0u32, |n, c| n * 26 + (c as u32) - ('a' as u32) + 1).saturating_sub(1)
}

fn physical_number_from_device(device: &str) -> Option<u32> {
    let lower = device.to_lowercase();
    if let Some(rest) = lower.strip_prefix("/dev/pd").or_else(|| lower.strip_prefix("pd")) {
        rest.trim().parse().ok()
    } else if let Some(rest) = lower.strip_prefix("/dev/sd").or_else(|| lower.strip_prefix("/dev/hd")) {
        Some(sd_letter_to_number(rest.trim()))
    } else if let Some(rest) = lower.strip_prefix(r"\\.\physicaldrive") {
        rest.trim().parse().ok()
    } else {
        None
    }
}

#[derive(Deserialize)]
struct ScanJson {
    #[serde(default)]
    devices: Vec<ScanDevice>,
}

#[derive(Deserialize)]
struct ScanDevice {
    name: String,
    #[serde(rename = "type")]
    dtype: String,
}

/// Parses `smartctl --scan -j` into (physical drive number, device, device type).
fn parse_scan_json(text: &str) -> Vec<(u32, String, String)> {
    serde_json::from_str::<ScanJson>(text)
        .map(|scan| {
            scan.devices
                .into_iter()
                .filter_map(|d| physical_number_from_device(&d.name).map(|n| (n, d.name, d.dtype)))
                .collect()
        })
        .unwrap_or_default()
}

fn run_smartctl(smartctl: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(smartctl)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run smartctl: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn normalize_serial(s: &str) -> String {
    s.to_lowercase().trim().trim_end_matches('.').replace(' ', "")
}

// ── smartctl JSON ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct SmartJson {
    #[serde(default)]
    smartctl: SmartctlMeta,
    #[serde(default)]
    serial_number: Option<String>,
    #[serde(default)]
    ata_smart_data: Option<AtaSmartData>,
    #[serde(default)]
    ata_smart_self_test_log: Option<AtaSelfTestLog>,
    #[serde(default)]
    nvme_self_test_log: Option<NvmeSelfTestLog>,
}

#[derive(Debug, Default, Deserialize)]
struct SmartctlMeta {
    #[serde(default)]
    exit_status: i32,
    #[serde(default)]
    output: Vec<String>,
    #[serde(default)]
    messages: Vec<SmartMessage>,
}

#[derive(Debug, Deserialize)]
struct SmartMessage {
    #[serde(default)]
    string: String,
}

#[derive(Debug, Deserialize)]
struct AtaSmartData {
    #[serde(default)]
    self_test: Option<AtaSelfTest>,
}

#[derive(Debug, Deserialize)]
struct AtaSelfTest {
    status: AtaSelfTestStatus,
}

#[derive(Debug, Deserialize)]
struct AtaSelfTestStatus {
    value: u32,
    #[serde(default)]
    string: String,
    #[serde(default)]
    remaining_percent: Option<u8>,
    #[serde(default)]
    passed: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AtaSelfTestLog {
    #[serde(default)]
    standard: Option<AtaLogTable>,
    #[serde(default)]
    extended: Option<AtaLogTable>,
}

#[derive(Debug, Deserialize)]
struct AtaLogTable {
    #[serde(default)]
    table: Vec<AtaLogEntry>,
}

#[derive(Debug, Deserialize)]
struct AtaLogEntry {
    #[serde(rename = "type")]
    test: Named,
    status: AtaLogStatus,
    #[serde(default)]
    lifetime_hours: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct Named {
    #[serde(default)]
    value: u32,
    #[serde(default)]
    string: String,
}

#[derive(Debug, Deserialize)]
struct AtaLogStatus {
    #[serde(default)]
    string: String,
    #[serde(default)]
    passed: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct NvmeSelfTestLog {
    #[serde(default)]
    current_self_test_operation: Option<Named>,
    #[serde(default)]
    current_self_test_completion_percent: Option<u8>,
    #[serde(default)]
    table: Vec<NvmeLogEntry>,
}

#[derive(Debug, Deserialize)]
struct NvmeLogEntry {
    self_test_code: Named,
    self_test_result: Named,
    #[serde(default)]
    power_on_hours: Option<u64>,
}

/// smartctl exit status bits 0–2 are errors; bits 3–7 describe the disk's health.
const EXIT_COMMAND_LINE: i32 = 1 << 0;
const EXIT_OPEN_FAILED: i32 = 1 << 1;
const EXIT_COMMAND_FAILED: i32 = 1 << 2;

fn parse_json(raw: &str) -> Result<SmartJson, String> {
    // The elevated helper may append stderr lines; the JSON object spans the outermost braces.
    match (raw.find('{'), raw.rfind('}')) {
        (Some(start), Some(end)) if end > start => {
            serde_json::from_str(&raw[start..=end]).map_err(|e| format!("Failed to parse smartctl output: {e}"))
        }
        _ => Err(format!("smartctl returned no JSON output: {}", raw.trim())),
    }
}

impl SmartJson {
    fn text(&self) -> String {
        self.smartctl.output.join("\n")
    }

    fn message(&self) -> String {
        let messages: Vec<&str> = self.smartctl.messages.iter().map(|m| m.string.as_str()).collect();
        if messages.is_empty() {
            format!("smartctl exited with status {}", self.smartctl.exit_status)
        } else {
            messages.join("; ")
        }
    }

    /// Turns the error bits of the exit status into a [`CommandError`]. A device that can't be
    /// opened without administrator rights becomes [`CommandError::NeedsElevation`]. Failed
    /// SMART commands only count when `strict` (e.g. starting a test); reports stay readable.
    fn check(&self, elevated: bool, strict: bool) -> Result<(), CommandError> {
        let status = self.smartctl.exit_status;
        if status & EXIT_OPEN_FAILED != 0 {
            return Err(if elevated {
                CommandError::from(format!("smartctl could not open the device: {}", self.message()))
            } else {
                CommandError::NeedsElevation { message: self.message() }
            });
        }
        let errors = if strict { EXIT_COMMAND_LINE | EXIT_COMMAND_FAILED } else { EXIT_COMMAND_LINE };
        if status & errors != 0 {
            return Err(CommandError::from(self.message()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SelfTestEntry {
    pub test: String,
    pub result: String,
    /// `None` when the result is neither a pass nor a failure (e.g. aborted).
    pub passed: Option<bool>,
    pub hours: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SelfTestStatus {
    pub in_progress: bool,
    pub percent_remaining: Option<u8>,
    pub status: String,
    pub failed: bool,
    /// Most recent first.
    pub log: Vec<SelfTestEntry>,
}

/// NVMe self-test result codes: 0 = no error, 5–7 = failed segment/LBA, others = aborted.
fn nvme_passed(code: u32) -> Option<bool> {
    match code {
        0 => Some(true),
        5..=7 => Some(false),
        _ => None,
    }
}

fn self_test_status(report: &SmartJson) -> SelfTestStatus {
    if let Some(nvme) = &report.nvme_self_test_log {
        let operation = nvme.current_self_test_operation.as_ref();
        let in_progress = operation.is_some_and(|op| op.value != 0);
        let log: Vec<SelfTestEntry> = nvme
            .table
            .iter()
            .map(|e| SelfTestEntry {
                test: e.self_test_code.string.clone(),
                result: e.self_test_result.string.clone(),
                passed: nvme_passed(e.self_test_result.value),
                hours: e.power_on_hours,
            })
            .collect();
        return SelfTestStatus {
            in_progress,
            percent_remaining: nvme
                .current_self_test_completion_percent
                .filter(|_| in_progress)
                .map(|done| 100u8.saturating_sub(done)),
            status: operation.map_or_else(|| "No self-test in progress".to_string(), |op| op.string.clone()),
            failed: !in_progress && log.first().and_then(|e| e.passed) == Some(false),
            log,
        };
    }

    let status = report.ata_smart_data.as_ref().and_then(|d| d.self_test.as_ref()).map(|t| &t.status);
    // ATA: a high nibble of 0xF means "in progress"; the low nibble is the remaining tenths.
    let in_progress = status.is_some_and(|s| s.value >> 4 == 0xF);
    let log: Vec<SelfTestEntry> = report
        .ata_smart_self_test_log
        .as_ref()
        .and_then(|l| l.standard.as_ref().or(l.extended.as_ref()))
        .map(|t| {
            t.table
                .iter()
                .map(|e| SelfTestEntry {
                    test: e.test.string.clone(),
                    result: e.status.string.clone(),
                    passed: e.status.passed,
                    hours: e.lifetime_hours,
                })
                .collect()
        })
        .unwrap_or_default();

    SelfTestStatus {
        in_progress,
        percent_remaining: status
            .filter(|_| in_progress)
            .map(|s| s.remaining_percent.unwrap_or(((s.value & 0xF) * 10) as u8)),
        status: status.map_or_else(|| "Self-test status not available".to_string(), |s| s.string.clone()),
        failed: !in_progress
            && (status.and_then(|s| s.passed) == Some(false) || log.first().and_then(|e| e.passed) == Some(false)),
        log,
    }
}

// ── Running smartctl on a drive ─────────────────────────────────────────────

fn run_on_device(
    smartctl: &std::path::Path,
    elevated: bool,
    args: &[&str],
    device: &str,
    dtype: &str,
) -> Result<SmartJson, CommandError> {
    let mut full: Vec<&str> = vec!["--json=o"];
    full.extend_from_slice(args);
    full.extend(["-d", dtype, device]);
    let raw = if elevated {
        elevated_helper::run_smartctl_elevated(smartctl, &full)?
    } else {
        run_smartctl(smartctl, &full)?
    };
    Ok(parse_json(&raw)?)
}

/// Runs smartctl `args` against a Windows drive id (`\\.\PHYSICALDRIVEn`).
fn run_on_drive(id: &str, elevated: bool, args: &[&str]) -> Result<SmartJson, CommandError> {
    let smartctl = ensure_smartctl()?;

    if let Some((device, dtype)) = cached_device(id) {
        match run_on_device(&smartctl, elevated, args, &device, &dtype) {
            Ok(report) => return Ok(report),
            Err(CommandError::ElevationDeclined) => return Err(CommandError::ElevationDeclined),
            Err(e) => {
                log::warn!("Cached smartctl device {device} for {id} failed ({e}); resolving it again");
                forget_device(id);
            }
        }
    }

    let number = extract_physical_drive_number(id)?;
    let expected_serial = normalize_serial(&crate::find_drive(id)?.serial);

    // Prefer the device smartctl --scan reports for this drive, verified by serial number
    // (falling back to the scan entry with the same drive number).
    let scan = parse_scan_json(&run_smartctl(&smartctl, &["--scan", "-j"]).unwrap_or_default());
    let by_serial = scan.iter().find(|(_, device, dtype)| {
        run_smartctl(&smartctl, &["-i", "-j", "-d", dtype, device])
            .ok()
            .and_then(|raw| parse_json(&raw).ok())
            .and_then(|info| info.serial_number)
            .is_some_and(|sn| normalize_serial(&sn) == expected_serial)
    });
    if let Some((_, device, dtype)) = by_serial.or_else(|| scan.iter().find(|(n, _, _)| *n == number)) {
        let report = run_on_device(&smartctl, elevated, args, device, dtype)?;
        remember_device(id, device, dtype);
        return Ok(report);
    }

    // Fall back to common device types on the /dev/pdN alias.
    let pd_path = format!("/dev/pd{number}");
    let mut last = None;
    for dtype in ["sat", "nvme", "ata", "scsi"] {
        let report = run_on_device(&smartctl, elevated, args, &pd_path, dtype)?;
        if report.smartctl.exit_status & EXIT_OPEN_FAILED == 0 {
            remember_device(id, &pd_path, dtype);
            return Ok(report);
        }
        last = Some(report);
    }
    last.ok_or_else(|| CommandError::from("smartctl did not produce output for this drive"))
}

// ── Commands ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SelfTestType {
    Short,
    Long,
    Conveyance,
    Offline,
}

impl SelfTestType {
    fn as_arg(self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::Long => "long",
            Self::Conveyance => "conveyance",
            Self::Offline => "offline",
        }
    }
}

/// The full SMART report (`smartctl -a`) as text.
#[tauri::command]
pub async fn get_drive_smart(id: String, elevated: bool) -> Result<String, CommandError> {
    util::logged_blocking("get_drive_smart", move || {
        let report = run_on_drive(&id, elevated, &["-a"])?;
        report.check(elevated, false)?;
        Ok(report.text())
    })
    .await
}

/// Starts a self-test (with administrator rights) and returns smartctl's message.
#[tauri::command]
pub async fn run_drive_test(id: String, test_type: SelfTestType) -> Result<String, CommandError> {
    util::logged_blocking("run_drive_test", move || {
        let report = run_on_drive(&id, true, &["-t", test_type.as_arg()])?;
        report.check(true, true)?;
        Ok(report.text())
    })
    .await
}

/// Current self-test progress and the self-test log (with administrator rights).
#[tauri::command]
pub async fn get_drive_self_test_status(id: String) -> Result<SelfTestStatus, CommandError> {
    util::logged_blocking("get_drive_self_test_status", move || {
        let report = run_on_drive(&id, true, &["-c", "-l", "selftest"])?;
        report.check(true, false)?;
        Ok(self_test_status(&report))
    })
    .await
}

/// Saves the full SMART report under `%APPDATA%\mula\snapshots\<drive>\` and returns the path.
#[tauri::command]
pub async fn save_smart_snapshot(id: String, label: String) -> Result<String, CommandError> {
    util::logged_blocking("save_smart_snapshot", move || {
        let report = run_on_drive(&id, true, &["-a"])?;
        report.check(true, false)?;

        let drive_dir = util::mula_config_dir()
            .ok_or("Could not determine config directory")?
            .join("snapshots")
            .join(util::safe_file_name(&id, "-_"));
        std::fs::create_dir_all(&drive_dir).map_err(|e| format!("Failed to create snapshot directory: {e}"))?;

        let timestamp = Local::now().format("%Y%m%d_%H%M%S");
        let path = drive_dir.join(format!("smart_{}_{timestamp}.txt", util::safe_file_name(&label, "-_")));
        std::fs::write(&path, report.text()).map_err(|e| format!("Failed to write snapshot file: {e}"))?;
        Ok(path.to_string_lossy().to_string())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(json: serde_json::Value) -> SmartJson {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn physical_drive_numbers_are_extracted_from_device_ids() {
        assert_eq!(extract_physical_drive_number(r"\\.\PHYSICALDRIVE2"), Ok(2));
        assert_eq!(extract_physical_drive_number(r"\\.\physicaldrive13"), Ok(13));
        assert_eq!(extract_physical_drive_number("PHYSICALDRIVE0"), Ok(0));
        assert!(extract_physical_drive_number("C:").is_err());
        assert!(extract_physical_drive_number(r"\\.\PHYSICALDRIVEX").is_err());
    }

    #[test]
    fn smartctl_device_names_map_to_physical_drive_numbers() {
        assert_eq!(sd_letter_to_number("a"), 0);
        assert_eq!(sd_letter_to_number("aa"), 26);
        assert_eq!(physical_number_from_device("/dev/sdc"), Some(2));
        assert_eq!(physical_number_from_device("/dev/hdb"), Some(1));
        assert_eq!(physical_number_from_device("/dev/pd3"), Some(3));
        assert_eq!(physical_number_from_device("pd7"), Some(7));
        assert_eq!(physical_number_from_device(r"\\.\PhysicalDrive4"), Some(4));
        assert_eq!(physical_number_from_device("/dev/nvme0"), None);
    }

    #[test]
    fn scan_json_is_parsed() {
        let scan = r#"{"devices": [
            {"name": "/dev/sda", "info_name": "/dev/sda", "type": "ata", "protocol": "ATA"},
            {"name": "/dev/nvme0", "type": "nvme"},
            {"name": "/dev/sdb", "type": "sat"}
        ]}"#;
        assert_eq!(
            parse_scan_json(scan),
            vec![(0, "/dev/sda".to_string(), "ata".to_string()), (1, "/dev/sdb".to_string(), "sat".to_string())]
        );
        assert!(parse_scan_json("not json").is_empty());
    }

    #[test]
    fn serials_are_normalized_for_comparison() {
        assert_eq!(normalize_serial("  WD-ABC 123. "), "wd-abc123");
    }

    #[test]
    fn json_is_found_even_with_extra_lines_around_it() {
        let parsed = parse_json("warning on stderr\r\n{\"smartctl\": {\"exit_status\": 0, \"output\": [\"a\", \"b\"]}}\r\n").unwrap();
        assert_eq!(parsed.text(), "a\nb");
        assert!(parse_json("Access denied").is_err());
    }

    #[test]
    fn open_failures_need_elevation_only_when_not_elevated() {
        let r = report(serde_json::json!({ "smartctl": { "exit_status": 2, "messages": [{ "string": "Open failed: Access denied" }] } }));
        assert_eq!(r.check(false, false), Err(CommandError::NeedsElevation { message: "Open failed: Access denied".into() }));
        assert!(matches!(r.check(true, false), Err(CommandError::Other { .. })));
    }

    #[test]
    fn health_bits_are_not_errors_and_command_failures_only_count_when_strict() {
        let failing_disk = report(serde_json::json!({ "smartctl": { "exit_status": 8 | 32 } }));
        assert_eq!(failing_disk.check(true, true), Ok(()));
        let checksum = report(serde_json::json!({ "smartctl": { "exit_status": 4 } }));
        assert_eq!(checksum.check(true, false), Ok(()));
        assert!(checksum.check(true, true).is_err());
        assert!(report(serde_json::json!({ "smartctl": { "exit_status": 1 } })).check(true, false).is_err());
    }

    #[test]
    fn ata_self_test_in_progress() {
        let s = self_test_status(&report(serde_json::json!({
            "ata_smart_data": { "self_test": { "status": { "value": 249, "string": "in progress, 90% remaining", "remaining_percent": 90 } } },
            "ata_smart_self_test_log": { "standard": { "table": [
                { "type": { "value": 1, "string": "Short offline" }, "status": { "value": 0, "string": "Completed without error", "passed": true }, "lifetime_hours": 1200 }
            ] } }
        })));
        assert!(s.in_progress && !s.failed);
        assert_eq!(s.percent_remaining, Some(90));
        assert_eq!(s.status, "in progress, 90% remaining");
        assert_eq!(s.log, vec![SelfTestEntry { test: "Short offline".into(), result: "Completed without error".into(), passed: Some(true), hours: Some(1200) }]);
    }

    #[test]
    fn ata_progress_falls_back_to_the_status_nibble() {
        let s = self_test_status(&report(serde_json::json!({
            "ata_smart_data": { "self_test": { "status": { "value": 244, "string": "in progress" } } }
        })));
        assert_eq!(s.percent_remaining, Some(40));
    }

    #[test]
    fn ata_failed_self_test() {
        let s = self_test_status(&report(serde_json::json!({
            "ata_smart_data": { "self_test": { "status": { "value": 121, "string": "completed: read failure", "passed": false } } },
            "ata_smart_self_test_log": { "standard": { "table": [
                { "type": { "string": "Extended offline" }, "status": { "string": "Completed: read failure", "passed": false }, "lifetime_hours": 5 }
            ] } }
        })));
        assert!(!s.in_progress && s.failed);
        assert_eq!(s.percent_remaining, None);
    }

    #[test]
    fn nvme_self_test_status() {
        let running = self_test_status(&report(serde_json::json!({
            "nvme_self_test_log": {
                "current_self_test_operation": { "value": 1, "string": "Short self-test in progress" },
                "current_self_test_completion_percent": 30,
                "table": []
            }
        })));
        assert!(running.in_progress);
        assert_eq!(running.percent_remaining, Some(70));

        let finished = self_test_status(&report(serde_json::json!({
            "nvme_self_test_log": {
                "current_self_test_operation": { "value": 0, "string": "No self-test in progress" },
                "table": [
                    { "self_test_code": { "value": 2, "string": "Extended" }, "self_test_result": { "value": 7, "string": "Completed: failed segments" }, "power_on_hours": 900 },
                    { "self_test_code": { "value": 1, "string": "Short" }, "self_test_result": { "value": 0, "string": "Completed without error" } }
                ]
            }
        })));
        assert!(!finished.in_progress && finished.failed);
        assert_eq!(finished.log[0].passed, Some(false));
        assert_eq!(finished.log[1].passed, Some(true));
        assert_eq!(nvme_passed(1), None, "an aborted test is neither a pass nor a failure");
    }

    #[test]
    fn missing_self_test_data_is_reported_as_unavailable() {
        let s = self_test_status(&SmartJson::default());
        assert!(!s.in_progress && !s.failed && s.log.is_empty());
        assert_eq!(s.status, "Self-test status not available");
    }

    #[test]
    fn self_test_types_are_validated_by_serde() {
        let t: SelfTestType = serde_json::from_str("\"long\"").unwrap();
        assert_eq!(t.as_arg(), "long");
        assert!(serde_json::from_str::<SelfTestType>("\"select,0-10\"").is_err());
        assert!(serde_json::from_str::<SelfTestType>("\"SHORT\"").is_err());
    }

    #[test]
    fn resolved_devices_are_cached_per_drive() {
        let id = r"\\.\PHYSICALDRIVE_TEST_CACHE";
        assert_eq!(cached_device(id), None);
        remember_device(id, "/dev/sdb", "sat");
        assert_eq!(cached_device(id), Some(("/dev/sdb".to_string(), "sat".to_string())));
        forget_device(id);
        assert_eq!(cached_device(id), None);
    }
}
