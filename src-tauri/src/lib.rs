use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

mod logger;

static VSD_PROCESS: Mutex<Option<Child>> = Mutex::new(None);
static VSD_LOG_BUFFER: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[derive(Serialize)]
pub struct AppInfo {
    version: String,
    config_path: String,
    platform: String,
    arch: String,
}

#[tauri::command]
fn get_app_info() -> AppInfo {
    let config_dir = dirs::config_dir()
        .map(|p| p.join("mula").to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    AppInfo {
        version: format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("BUILD_TIMESTAMP")),
        config_path: config_dir,
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

#[tauri::command]
fn open_config_dir() -> Result<String, String> {
    let config_dir = dirs::config_dir()
        .map(|p| p.join("mula"))
        .ok_or("Could not determine config directory")?;

    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("Failed to create config directory: {e}"))?;

    Ok(config_dir.to_string_lossy().to_string())
}

// ── VSD Server commands ──

fn get_vsd_server_path() -> Option<std::path::PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();

    let candidates: Vec<std::path::PathBuf> = vec![
        // Development: relative to project root (exe is in src-tauri/target/debug/)
        exe_dir.join("../../../modules/vsd/server.py"),
        // Installed: next to the executable
        exe_dir.join("modules/vsd/server.py"),
    ];

    // Also check legacy location
    let mut all_candidates = candidates;
    if let Some(home) = dirs::home_dir() {
        all_candidates.push(home.join("CascadeProjects").join("VSD Experiment").join("companion").join("server.py"));
    }

    for candidate in all_candidates {
        if candidate.exists() {
            return candidate.canonicalize().ok().or(Some(candidate));
        }
    }
    None
}

fn get_python_command() -> String {
    // Try python3 first, fall back to python
    if cfg!(target_os = "windows") {
        "python".to_string()
    } else {
        "python3".to_string()
    }
}

#[tauri::command]
async fn vsd_start() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut proc_guard = VSD_PROCESS.lock().map_err(|e| e.to_string())?;

        if proc_guard.is_some() {
            return Err("VSD Server is already running".to_string());
        }

    let server_path = get_vsd_server_path()
        .ok_or("Could not find VSD server.py")?;

    let python = get_python_command();
    let working_dir = server_path.parent().unwrap().to_path_buf();

    // Install dependencies if needed
    let requirements = working_dir.join("requirements.txt");
    if requirements.exists() {
        if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
            logs.clear();
            logs.push("[Checking dependencies...]\n".to_string());
        }

        let install = Command::new(&python)
            .args(["-m", "pip", "install", "-r"])
            .arg(&requirements)
            .arg("--quiet")
            .current_dir(&working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        match install {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
                        logs.push(format!("[pip install failed: {}]\n", stderr.trim()));
                    }
                    return Err(format!("Failed to install dependencies: {}", stderr.trim()));
                }
                if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
                    logs.push("[Dependencies OK]\n".to_string());
                }
            }
            Err(e) => {
                return Err(format!("Failed to run pip: {e}"));
            }
        }
    } else {
        // Clear log buffer
        if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
            logs.clear();
        }
    }

    // Pass configured download dir to server via env var
    let download_dir = vsd_get_download_dir();

    let mut child = Command::new(&python)
        .arg(&server_path)
        .current_dir(&working_dir)
        .env("VSD_DOWNLOAD_DIR", &download_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start VSD server: {e}"))?;

    // Spawn thread to read stdout
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if let Ok(line) = line {
                    log::info!("[VSD] {line}");
                    if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
                        logs.push(format!("{line}\n"));
                        if logs.len() > 1000 {
                            logs.drain(0..100);
                        }
                    }
                }
            }
        });
    }

    // Spawn thread to read stderr
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                if let Ok(line) = line {
                    log::info!("[VSD] {line}");
                    if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
                        logs.push(format!("{line}\n"));
                        if logs.len() > 1000 {
                            logs.drain(0..100);
                        }
                    }
                }
            }
        });
    }

        *proc_guard = Some(child);
        Ok(())
    })
    .await
    .map_err(|e| format!("Background task failed: {e}"))?
}

#[tauri::command]
async fn vsd_stop() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
    let mut proc_guard = VSD_PROCESS.lock().map_err(|e| e.to_string())?;

        if let Some(mut child) = proc_guard.take() {
            child.kill().map_err(|e| format!("Failed to stop VSD server: {e}"))?;
            let _ = child.wait();
            Ok(())
        } else {
            Err("VSD Server is not running".to_string())
        }
    })
    .await
    .map_err(|e| format!("Background task failed: {e}"))?
}

fn vsd_is_running_sync() -> bool {
    if let Ok(mut proc_guard) = VSD_PROCESS.lock() {
        if let Some(child) = proc_guard.as_mut() {
            // Check if process is still alive
            match child.try_wait() {
                Ok(None) => true,     // still running
                Ok(Some(_)) => {
                    *proc_guard = None; // process exited
                    false
                }
                Err(_) => false,
            }
        } else {
            false
        }
    } else {
        false
    }
}

#[tauri::command]
async fn vsd_is_running() -> bool {
    tauri::async_runtime::spawn_blocking(vsd_is_running_sync)
        .await
        .unwrap_or(false)
}

/// Returns new log lines since last call (drains the buffer)
#[tauri::command]
fn vsd_get_logs() -> Vec<String> {
    if let Ok(mut logs) = VSD_LOG_BUFFER.lock() {
        let lines: Vec<String> = logs.drain(..).collect();
        lines
    } else {
        vec![]
    }
}

/// Read a key from vsd.conf
fn read_vsd_config_value(key: &str) -> Option<String> {
    let config_path = get_config_dir().join("vsd.conf");
    if config_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            let prefix = format!("{key}=");
            for line in content.lines() {
                if let Some(val) = line.strip_prefix(&prefix) {
                    return Some(val.trim().to_string());
                }
            }
        }
    }
    None
}

/// Write a key=value to vsd.conf (update if exists, append if not)
fn write_vsd_config_value(key: &str, value: &str) -> Result<(), String> {
    let config_path = get_config_dir().join("vsd.conf");
    let config_dir = config_path.parent().unwrap();
    std::fs::create_dir_all(config_dir)
        .map_err(|e| format!("Failed to create config dir: {e}"))?;

    let mut lines: Vec<String> = if config_path.exists() {
        std::fs::read_to_string(&config_path)
            .unwrap_or_default()
            .lines()
            .map(|l| l.to_string())
            .collect()
    } else {
        vec![]
    };

    let new_line = format!("{key}={value}");
    let prefix = format!("{key}=");
    let mut found = false;
    for line in lines.iter_mut() {
        if line.starts_with(&prefix) {
            *line = new_line.clone();
            found = true;
            break;
        }
    }
    if !found {
        lines.push(new_line);
    }

    std::fs::write(&config_path, lines.join("\n") + "\n")
        .map_err(|e| format!("Failed to write config: {e}"))?;
    Ok(())
}

/// Get the MULA config directory (e.g. C:\Users\<user>\AppData\Roaming\mula)
fn get_config_dir() -> std::path::PathBuf {
    dirs::config_dir()
        .map(|p| p.join("mula"))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

#[tauri::command]
fn vsd_get_download_dir() -> String {
    read_vsd_config_value("VSD_DOWNLOAD_DIR").unwrap_or_else(|| {
        dirs::home_dir()
            .map(|h| h.join("Downloads").join("VSD").to_string_lossy().to_string())
            .unwrap_or_default()
    })
}

#[tauri::command]
fn vsd_set_download_dir(path: String) -> Result<(), String> {
    write_vsd_config_value("VSD_DOWNLOAD_DIR", &path)?;

    // If server is running, update it live via API
    if vsd_is_running_sync() {
        let body = format!(r#"{{"path":"{}"}}"#, path.replace('\\', "\\\\").replace('"', "\\\""));
        let _ = std::thread::spawn(move || {
            let req = urllib_post("http://127.0.0.1:8765/config/download_dir", &body);
            if let Err(e) = req {
                log::error!("Failed to update running server download dir: {e}");
            }
        });
    }

    Ok(())
}

#[tauri::command]
fn vsd_get_autostart() -> bool {
    read_vsd_config_value("VSD_AUTOSTART")
        .map(|v| v == "true")
        .unwrap_or(false)
}

#[tauri::command]
fn vsd_set_autostart(enabled: bool) -> Result<(), String> {
    write_vsd_config_value("VSD_AUTOSTART", if enabled { "true" } else { "false" })
}

fn urllib_post(url: &str, body: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let addr = "127.0.0.1:8765";
    let mut stream = TcpStream::connect(addr).map_err(|e| e.to_string())?;
    let request = format!(
        "POST /config/download_dir HTTP/1.1\r\nHost: 127.0.0.1:8765\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    );
    stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream.read_to_string(&mut response).map_err(|e| e.to_string())?;
    let _ = url; // used in format above
    Ok(())
}

/// Get the extension directory for a given browser target
fn get_extension_dir(browser: &str) -> Option<std::path::PathBuf> {
    let vsd_dir = get_vsd_server_path()?.parent()?.to_path_buf();
    let ext_dir = vsd_dir.join("extension").join("dist").join(browser);
    Some(ext_dir)
}

/// Get the extension build script path
fn get_extension_build_script() -> Option<std::path::PathBuf> {
    let vsd_dir = get_vsd_server_path()?.parent()?.to_path_buf();
    let build_script = vsd_dir.join("extension").join("build.py");
    if build_script.exists() { Some(build_script) } else { None }
}

#[tauri::command]
async fn vsd_install_extension(browser: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
    let build_script = get_extension_build_script()
        .ok_or("Could not find extension build script")?;

    let python = get_python_command();

    // Build the extension
    let output = Command::new(&python)
        .arg(&build_script)
        .arg(&browser)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run build script: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Build failed: {}", stderr.trim()));
    }

    let ext_dir = get_extension_dir(&browser)
        .ok_or("Could not determine extension output directory")?;

    if !ext_dir.exists() {
        return Err(format!("Extension not found at {}", ext_dir.display()));
    }

    // Open the built extension folder in the file explorer
    if cfg!(target_os = "windows") {
        let _ = Command::new("explorer").arg(&ext_dir).spawn();
    } else if cfg!(target_os = "macos") {
        let _ = Command::new("open").arg(&ext_dir).spawn();
    } else {
        let _ = Command::new("xdg-open").arg(&ext_dir).spawn();
    }

    Ok(ext_dir.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| format!("Background task failed: {e}"))?
}

// ── Drive Test commands ──

#[derive(Serialize, Clone)]
pub struct DriveInfo {
    id: String,
    model: String,
    vendor: String,
    serial: String,
    #[serde(rename = "type")]
    drive_type: String,
    media_type: String,
    bus_type: String,
    interface_type: String,
    size: u64,
    size_text: String,
    partitions: u32,
    status: String,
    firmware: String,
    pnp_device_id: String,
    device_id: String,
    drive_letters: Vec<String>,
    mount_points: Vec<String>,
    health_status: String,
    connection_speed: String,
    smart_capable: String,
    trim_capable: String,
}

fn parse_vendor_from_pnp(pnp: &str) -> Option<String> {
    // e.g. SCSI\DISK&VEN_WDC&PROD_WD3003FZEX-00Z4S\...
    if let Some(start) = pnp.find("VEN_") {
        let rest = &pnp[start + 4..];
        let end = rest.find('&').or_else(|| rest.find('\\')).unwrap_or(rest.len());
        let vendor = &rest[..end];
        if !vendor.is_empty() {
            return Some(vendor.to_string());
        }
    }
    None
}

fn is_real_vendor(v: &str) -> bool {
    let v = v.trim().to_uppercase();
    if v.is_empty() {
        return false;
    }
    const NON_VENDORS: &[&str] = &[
        "NVME", "SCSI", "IDE", "SATA", "SAS", "USB", "RAID", "STORAGE",
        "MICROSOFT", "VBOX", "VMWARE", "QEMU", "HYPER", "VIRTUAL", "GENERIC",
        "INTEL RST", "AMD", "MARVELL", "LSI", "ADAPTEC", "BROADCOM",
    ];
    !NON_VENDORS.contains(&v.as_str())
}

fn guess_vendor_from_model(model: &str) -> Option<String> {
    if model.is_empty() {
        return None;
    }
    let first_token = model
        .split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or("")
        .to_uppercase();
    let prefix: String = first_token.chars().take_while(|c| c.is_ascii_alphabetic()).collect();

    // Exact full-token matches
    let exact_vendor = match first_token.as_str() {
        "SAMSUNG" => "Samsung",
        "INTEL" => "Intel",
        "TOSHIBA" => "Toshiba",
        "SANDISK" => "SanDisk",
        "KINGSTON" => "Kingston",
        "MICRON" => "Micron",
        "CRUCIAL" => "Crucial",
        "SEAGATE" => "Seagate",
        "MAXTOR" => "Maxtor",
        "HITACHI" => "Hitachi",
        "HGST" => "HGST",
        "ADATA" => "ADATA",
        "CORSAIR" => "Corsair",
        "TRANSCEND" => "Transcend",
        "LEXAR" => "Lexar",
        "APACER" => "Apacer",
        "KIOXIA" => "Kioxia",
        "SABRENT" => "Sabrent",
        "TEAM" => "Team",
        "ASU" => "ADATA",
        "XPG" => "ADATA",
        "SP" => "Silicon Power",
        "TS" => "Transcend",
        "CSSD" => "Corsair",
        "WDS" => "Western Digital",
        "T253" => "Team",
        "NM" => "Lexar",
        _ => "",
    };
    if !exact_vendor.is_empty() {
        return Some(exact_vendor.to_string());
    }

    // Prefix matches
    let vendor = match prefix.as_str() {
        "ST" | "STX" => "Seagate",
        "WD" | "WDC" => "Western Digital",
        "SK" | "SKHYNIX" | "HFS" | "HFM" => "SK hynix",
        "PNY" | "CS" => "PNY",
        "SSDPE" => "Intel",
        "CT" => "Crucial",
        "THNS" | "KXG" | "XQ" => "Toshiba",
        "SD" | "SDSS" => "SanDisk",
        "SA400" | "KC" | "SUV" | "SH" | "SKC" => "Kingston",
        "MTFD" | "MT" => "Micron",
        "HTS" | "HUA" => "Hitachi",
        "HUS" | "HUH" | "HM" => "HGST",
        "SU" | "ASU" | "XPG" => "ADATA",
        "SP" | "SPCC" => "Silicon Power",
        "TS" => "Transcend",
        "CSSD" => "Corsair",
        "WDS" => "Western Digital",
        "T253" => "Team",
        "NM" | "NQ" | "NS" => "Lexar",
        _ => return None,
    };
    Some(vendor.to_string())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut size = bytes as f64;
    let mut unit_index = 0;
    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }
    format!("{:.2} {}", size, UNITS[unit_index])
}

fn format_bitrate(bits_per_sec: u64) -> String {
    if bits_per_sec == 0 {
        return "Unknown".to_string();
    }
    // Some WMI sources report the value scaled down (e.g. Mbps or 100 Mbps).
    // Detect and normalize to bits/s for display.
    let bps = bits_per_sec as f64;
    if bps < 1_000_000.0 {
        // Possibly already in Mbps? Unlikely, but handle by leaving as-is.
    }
    const UNITS: &[&str] = &["bps", "Kbps", "Mbps", "Gbps", "Tbps"];
    let mut size = bps;
    let mut unit_index = 0;
    while size >= 1000.0 && unit_index < UNITS.len() - 1 {
        size /= 1000.0;
        unit_index += 1;
    }
    format!("{:.2} {}", size, UNITS[unit_index])
}

fn infer_connection_speed(bus_type: &str, interface_type: &str) -> String {
    let bus = bus_type.to_lowercase();
    let interface = interface_type.to_lowercase();
    if bus.contains("nvme") || interface.contains("nvme") {
        return "PCIe (varies by generation)".to_string();
    }
    if bus.contains("sata") || interface.contains("sata") {
        return "Up to 6 Gbps (SATA 6 Gb/s)".to_string();
    }
    if bus.contains("sas") || interface.contains("sas") {
        return "Up to 12 Gbps (SAS 12 Gb/s)".to_string();
    }
    if bus.contains("usb") || interface.contains("usb") {
        return "USB (varies by port)".to_string();
    }
    if bus.contains("ide") || interface.contains("ide") {
        return "Up to 133 MB/s (PATA)".to_string();
    }
    if bus.contains("scsi") || interface.contains("scsi") {
        return "SCSI (varies by generation)".to_string();
    }
    "Unknown".to_string()
}

#[cfg(target_os = "windows")]
fn list_physical_drives_impl() -> Result<Vec<DriveInfo>, String> {
    #[derive(Deserialize, Debug)]
    struct PsDiskDrive {
        #[serde(rename = "DeviceID", alias = "DeviceId")]
        device_id: String,
        #[serde(rename = "Model")]
        model: String,
        #[serde(rename = "SerialNumber")]
        serial_number: Option<String>,
        #[serde(rename = "Manufacturer")]
        manufacturer: Option<String>,
        #[serde(rename = "InterfaceType")]
        interface_type: Option<String>,
        #[serde(rename = "MediaType")]
        media_type: Option<String>,
        #[serde(rename = "Size")]
        size: Option<u64>,
        #[serde(rename = "Partitions")]
        partitions: Option<u32>,
        #[serde(rename = "Status")]
        status: Option<String>,
        #[serde(rename = "FirmwareRevision")]
        firmware_revision: Option<String>,
        #[serde(rename = "PNPDeviceID", alias = "PnpDeviceId")]
        pnp_device_id: Option<String>,
        #[serde(rename = "PhysicalMediaType")]
        physical_media_type: Option<String>,
        #[serde(rename = "PhysicalBusType")]
        physical_bus_type: Option<String>,
        #[serde(rename = "HealthStatus")]
        health_status: Option<String>,
        #[serde(rename = "DriveLetters")]
        drive_letters: Option<serde_json::Value>,
        #[serde(rename = "MountPoints")]
        mount_points: Option<serde_json::Value>,
        #[serde(rename = "ConnectionSpeed")]
        connection_speed: Option<u64>,
        #[serde(rename = "SmartCapable")]
        smart_capable: Option<String>,
        #[serde(rename = "TrimCapable")]
        trim_capable: Option<String>,
    }

    let script = r#"
        [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
        $OutputEncoding = [System.Text.Encoding]::UTF8
        Import-Module Storage -ErrorAction SilentlyContinue

        $physical = @{}
        try {
            Get-PhysicalDisk -ErrorAction SilentlyContinue | ForEach-Object {
                $physical[[string]$_.DeviceId] = @{
                    Model = $_.Model
                    SerialNumber = $_.SerialNumber
                    Manufacturer = $_.Manufacturer
                    MediaType = $_.MediaType
                    BusType = $_.BusType
                    FirmwareVersion = $_.FirmwareVersion
                    HealthStatus = $_.HealthStatus
                }
            }
        } catch {}

        $speeds = @{}
        try {
            $devs = Get-CimInstance -ClassName Win32_SCSIControllerDevice -ErrorAction SilentlyContinue
            foreach ($dev in $devs) {
                $key = if ($dev.Dependent.PNPDeviceID) { $dev.Dependent.PNPDeviceID } else { $dev.Dependent.DeviceID }
                if ($key -and $dev.NegotiatedSpeed) { $speeds[$key] = [uint64]$dev.NegotiatedSpeed }
            }
            $idevs = Get-CimInstance -ClassName Win32_IDEControllerDevice -ErrorAction SilentlyContinue
            foreach ($dev in $idevs) {
                $key = if ($dev.Dependent.PNPDeviceID) { $dev.Dependent.PNPDeviceID } else { $dev.Dependent.DeviceID }
                if ($key -and $dev.NegotiatedSpeed) { $speeds[$key] = [uint64]$dev.NegotiatedSpeed }
            }
        } catch {}

        $volumes = @{}
        try {
            Get-Partition -ErrorAction SilentlyContinue | ForEach-Object {
                $diskNum = [string]$_.DiskNumber
                if ($diskNum -eq $null) { continue }
                if (-not $volumes[$diskNum]) { $volumes[$diskNum] = @{ Letters = @(); Mounts = @() } }

                if ($_.DriveLetter) {
                    $letter = ($_.DriveLetter.ToString() + ':')
                    if (-not $volumes[$diskNum].Letters.Contains($letter)) { $volumes[$diskNum].Letters += $letter }
                }

                if ($_.AccessPaths) {
                    foreach ($ap in $_.AccessPaths) {
                        if ($ap -match '^([A-Za-z]):\\$') {
                            $letter = ($matches[1] + ':')
                            if (-not $volumes[$diskNum].Letters.Contains($letter)) { $volumes[$diskNum].Letters += $letter }
                        } elseif ($ap -and -not $ap.StartsWith('\\?\Volume')) {
                            $volumes[$diskNum].Mounts += $ap
                        }
                    }
                }
            }
        } catch {}

        $disks = Get-CimInstance -ClassName Win32_DiskDrive -ErrorAction SilentlyContinue |
            Select-Object DeviceID, Model, SerialNumber, Manufacturer, InterfaceType, MediaType, Size, Partitions, Status, FirmwareRevision, PNPDeviceID, Index

        $result = foreach ($d in $disks) {
            $ph = $null
            if ($d.Index -ne $null) { $ph = $physical[[string]$d.Index] }
            $vol = $null
            if ($d.Index -ne $null) { $vol = $volumes[[string]$d.Index] }

            $finalModel = if ($ph -and $ph.Model) { $ph.Model } else { $d.Model }
            $finalSerial = if ($ph -and $ph.SerialNumber) { $ph.SerialNumber } else { $d.SerialNumber }
            $finalFirmware = if ($ph -and $ph.FirmwareVersion) { $ph.FirmwareVersion } else { $d.FirmwareRevision }
            $finalMediaType = if ($ph -and $ph.MediaType) { $ph.MediaType } else { $d.MediaType }
            $finalBusType = if ($ph -and $ph.BusType) { $ph.BusType } else { $d.InterfaceType }
            $finalHealth = if ($ph -and $ph.HealthStatus) { $ph.HealthStatus } else { $null }

            [PSCustomObject]@{
                DeviceID = $d.DeviceID
                Model = $finalModel
                SerialNumber = $finalSerial
                Manufacturer = if ($ph -and $ph.Manufacturer) { $ph.Manufacturer } else { $d.Manufacturer }
                InterfaceType = $d.InterfaceType
                MediaType = $d.MediaType
                Size = [uint64]$d.Size
                Partitions = [uint32]$d.Partitions
                Status = $d.Status
                FirmwareRevision = $finalFirmware
                PNPDeviceID = $d.PNPDeviceID
                PhysicalMediaType = $finalMediaType
                PhysicalBusType = $finalBusType
                HealthStatus = $finalHealth
                DriveLetters = if ($vol -and $vol.Letters.Count -gt 0) { @($vol.Letters) } else { $null }
                MountPoints = if ($vol -and $vol.Mounts.Count -gt 0) { @($vol.Mounts) } else { $null }
                ConnectionSpeed = if ($d.PNPDeviceID -and $speeds[$d.PNPDeviceID]) { $speeds[$d.PNPDeviceID] } else { $null }
                SmartCapable = if (@('NVMe','SATA','SAS','SCSI','IDE','ATA','SATA','PCIe') -contains $finalBusType) { 'Yes' } else { 'Unknown' }
                TrimCapable = if ($finalMediaType -eq 'SSD' -and @('NVMe','SATA','SAS','SCSI','IDE','ATA','PCIe') -contains $finalBusType) { 'Yes' } elseif ($finalMediaType -eq 'HDD') { 'No' } else { 'Unknown' }
            }
        }

        $json = ConvertTo-Json -InputObject @($result) -Depth 3 -Compress
        Write-Output $json
    "#;

    let output = Command::new("powershell")
        .args(["-ExecutionPolicy", "Bypass", "-Command", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run PowerShell drive query: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("PowerShell drive query failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let drives: Vec<PsDiskDrive> = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse drive data: {e}"))?;

    let mut result = Vec::new();
    for d in drives {
        let pnp = d.pnp_device_id.as_deref().unwrap_or("");
        let pnp_vendor = parse_vendor_from_pnp(pnp).filter(|v| is_real_vendor(v));
        let manufacturer = d.manufacturer.as_ref().and_then(|m| {
            let trimmed = m.trim();
            if trimmed.is_empty() || trimmed.starts_with('(') || trimmed.to_lowercase().contains("standard") {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        let vendor = guess_vendor_from_model(&d.model)
            .or(pnp_vendor)
            .or(manufacturer)
            .unwrap_or_default();

        let model = d.model.trim().to_string();
        let serial = d.serial_number
            .as_deref()
            .unwrap_or("")
            .trim()
            .trim_end_matches('.')
            .to_string();
        let size = d.size.unwrap_or(0);

        let media_type = d.physical_media_type.as_deref().or(d.media_type.as_deref()).unwrap_or("Unknown").trim().to_string();
        let bus_type = d.physical_bus_type.as_deref().or(d.interface_type.as_deref()).unwrap_or("Unknown").trim().to_string();
        let drive_type = d.physical_media_type.as_deref().or(d.media_type.as_deref()).unwrap_or("Unknown").trim().to_string();

        let connection_speed_text = if d.connection_speed.unwrap_or(0) > 0 {
            format_bitrate(d.connection_speed.unwrap())
        } else {
            infer_connection_speed(&bus_type, d.interface_type.as_deref().unwrap_or(""))
        };

        fn value_to_strings(v: Option<serde_json::Value>) -> Vec<String> {
            match v {
                None => vec![],
                Some(serde_json::Value::Null) => vec![],
                Some(serde_json::Value::String(s)) => vec![s],
                Some(serde_json::Value::Array(arr)) => arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect(),
                Some(_) => vec![],
            }
        }

        result.push(DriveInfo {
            id: d.device_id.clone(),
            device_id: d.device_id,
            model,
            vendor,
            serial,
            drive_type,
            media_type,
            bus_type,
            interface_type: d.interface_type.as_deref().unwrap_or("Unknown").to_string(),
            size,
            size_text: format_bytes(size),
            partitions: d.partitions.unwrap_or(0),
            status: d.status.as_deref().unwrap_or("Unknown").to_string(),
            firmware: d.firmware_revision.as_deref().unwrap_or("").to_string(),
            pnp_device_id: pnp.to_string(),
            drive_letters: value_to_strings(d.drive_letters),
            mount_points: value_to_strings(d.mount_points),
            health_status: d.health_status.unwrap_or_default(),
            connection_speed: connection_speed_text,
            smart_capable: d.smart_capable.as_deref().unwrap_or("Unknown").to_string(),
            trim_capable: d.trim_capable.as_deref().unwrap_or("Unknown").to_string(),
        });
    }

    Ok(result)
}

#[cfg(not(target_os = "windows"))]
fn list_physical_drives_impl() -> Result<Vec<DriveInfo>, String> {
    Ok(vec![])
}

#[tauri::command]
async fn list_physical_drives() -> Result<Vec<DriveInfo>, String> {
    tauri::async_runtime::spawn_blocking(list_physical_drives_impl)
        .await
        .map_err(|e| format!("Background task failed: {e}"))?
}

#[tauri::command]
async fn get_drive_details(id: String) -> Result<DriveInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let drives = list_physical_drives_impl()?;
        drives.into_iter()
            .find(|d| d.id == id || d.device_id == id)
            .ok_or_else(|| format!("Drive not found: {id}"))
    })
    .await
    .map_err(|e| format!("Background task failed: {e}"))?
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

    num_str
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("Could not parse drive number from: {id}"))
}

fn find_smartctl() -> Option<std::path::PathBuf> {
    let known_paths = [
        r"C:\Program Files\smartmontools\bin\smartctl.exe",
        r"C:\Program Files (x86)\smartmontools\bin\smartctl.exe",
    ];
    for p in &known_paths {
        let path = std::path::PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    // Try PATH
    if Command::new("smartctl").arg("--version").output().is_ok() {
        return Some(std::path::PathBuf::from("smartctl"));
    }
    None
}

fn find_winget() -> Option<std::path::PathBuf> {
    if let Ok(output) = Command::new("where").arg("winget").output() {
        let out = String::from_utf8_lossy(&output.stdout);
        let first = out.lines().next().map(|s| s.trim().to_string());
        if let Some(p) = first {
            if !p.is_empty() && std::path::Path::new(&p).exists() {
                return Some(std::path::PathBuf::from(p));
            }
        }
    }
    // Try a direct winget invocation
    if Command::new("winget").arg("--version").output().is_ok() {
        return Some(std::path::PathBuf::from("winget"));
    }
    None
}

fn install_smartctl() -> Result<std::path::PathBuf, String> {
    if cfg!(not(target_os = "windows")) {
        return Err("smartctl not found. Please install smartmontools using your package manager.".to_string());
    }

    let winget = find_winget().ok_or("winget not found. Please install smartmontools manually from https://www.smartmontools.org/")?;

    let output = Command::new(&winget)
        .args([
            "install",
            "--id", "smartmontools.smartmontools",
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "winget install failed.\nstdout: {}\nstderr: {}",
            stdout.trim(),
            stderr.trim()
        ));
    }

    find_smartctl().ok_or_else(|| "smartmontools was installed but smartctl could not be found. Restart MULA or add C:\\Program Files\\smartmontools\\bin to PATH.".to_string())
}

fn ensure_smartctl() -> Result<std::path::PathBuf, String> {
    static SMARTCTL_PATH: std::sync::Mutex<Option<Result<std::path::PathBuf, String>>> = std::sync::Mutex::new(None);

    let mut guard = SMARTCTL_PATH.lock().map_err(|e| e.to_string())?;
    if let Some(result) = guard.as_ref() {
        return result.clone();
    }

    let result = find_smartctl()
        .map(Ok)
        .unwrap_or_else(|| install_smartctl())
        .clone();
    *guard = Some(result.clone());
    result
}

fn sd_letter_to_number(s: &str) -> u32 {
    let mut n = 0u32;
    for c in s.chars() {
        n = n * 26 + ((c as u32) - ('a' as u32) + 1);
    }
    n.saturating_sub(1)
}

fn physical_number_from_device(device: &str) -> Option<u32> {
    let lower = device.to_lowercase();
    if let Some(rest) = lower.strip_prefix("/dev/pd") {
        rest.trim().parse().ok()
    } else if let Some(rest) = lower.strip_prefix("pd") {
        rest.trim().parse().ok()
    } else if let Some(rest) = lower.strip_prefix("/dev/sd") {
        Some(sd_letter_to_number(rest.trim()))
    } else if let Some(rest) = lower.strip_prefix("/dev/hd") {
        Some(sd_letter_to_number(rest.trim()))
    } else if let Some(rest) = lower.strip_prefix(r"\\.\physicaldrive") {
        rest.trim().parse().ok()
    } else {
        None
    }
}

fn parse_smartctl_scan(smartctl: &std::path::Path) -> Result<Vec<(u32, String, String)>, String> {
    let output = Command::new(smartctl)
        .arg("--scan")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run smartctl --scan: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("smartctl --scan failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut devices = Vec::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 || parts[1] != "-d" {
            continue;
        }
        let device = parts[0].to_string();
        let dtype = parts[2].to_string();
        if let Some(number) = physical_number_from_device(&device) {
            devices.push((number, device, dtype));
        }
    }
    Ok(devices)
}

fn run_smartctl(smartctl: &std::path::Path, args: &[&str]) -> Result<(String, String, i32), String> {
    let output = Command::new(smartctl)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run smartctl: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    Ok((stdout, stderr, code))
}

fn parse_smartctl_serial(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let lower = line.to_lowercase();
        if let Some(idx) = lower.find("serial number") {
            let after = &line[idx + "serial number".len()..];
            let after = after.trim_start_matches(':').trim();
            if !after.is_empty() && after != "[no information found]" && after != "unknown" {
                return Some(after.to_string());
            }
        }
    }
    None
}

fn normalize_serial(s: &str) -> String {
    s.to_lowercase().trim().trim_end_matches('.').replace(' ', "")
}

#[tauri::command]
async fn get_drive_smart(id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let number = extract_physical_drive_number(&id)?;
        let pd_path = format!(r"/dev/pd{}", number);

        let smartctl = ensure_smartctl()?;

        // Look up the drive we already enumerated so we can match by serial.
        let drives = list_physical_drives_impl()?;
        let drive = drives
            .into_iter()
            .find(|d| d.id == id || d.device_id == id)
            .ok_or_else(|| format!("Drive not found: {id}"))?;
        let expected_serial = normalize_serial(&drive.serial);

    // Prefer the device and type reported by smartctl --scan, but verify the
    // serial before trusting the mapping.
    let scan = parse_smartctl_scan(&smartctl).unwrap_or_default();
    let mut matched: Option<(String, String)> = None;

    for (_n, device, dtype) in &scan {
        let (info, _err, _code) = run_smartctl(&smartctl, &["-i", "-d", dtype, device])?;
        if let Some(sn) = parse_smartctl_serial(&info) {
            if normalize_serial(&sn) == expected_serial {
                matched = Some((device.clone(), dtype.clone()));
                break;
            }
        }
    }

    // If no serial match, fall back to the scan entry with the same
    // physical number, then to brute-force device types on /dev/pdN.
    if matched.is_none() {
        if let Some((_, device, dtype)) = scan.iter().find(|(n, _, _)| *n == number) {
            matched = Some((device.clone(), dtype.clone()));
        }
    }

    if let Some((device, dtype)) = matched {
        let (stdout, stderr, _code) = run_smartctl(&smartctl, &["-a", "-d", &dtype, &device])?;
        if !stdout.trim().is_empty() {
            return Ok(stdout);
        }
        if !stderr.trim().is_empty() {
            return Err(format!("smartctl error: {}", stderr.trim()));
        }
    }

    // Fallback: try common device types on the /dev/pdN alias.
    let fallback_types = ["sat", "nvme", "ata", "scsi"];
    let mut last_error = String::new();
    for dtype in &fallback_types {
        let (stdout, stderr, _code) = run_smartctl(&smartctl, &["-a", "-d", dtype, &pd_path])?;
        if !stdout.trim().is_empty() && !stderr.contains("failed") && !stderr.contains("Open failed") {
            return Ok(stdout);
        }
        if !stderr.trim().is_empty() {
            last_error = stderr.trim().to_string();
        }
    }

    if !last_error.is_empty() {
        return Err(format!("smartctl error: {}", last_error));
    }

        Err("No SMART data available for this drive.".to_string())
    })
    .await
    .map_err(|e| format!("Background task failed: {e}"))?
}

#[tauri::command]
fn log_message(level: String, message: String) {
    logger::log_message(&level, &message);
}

#[tauri::command]
fn restart_as_admin() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Could not determine application path: {e}"))?;
    let exe_str = exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let ps = format!("Start-Process -FilePath \"{}\" -Verb runAs", exe_str);

    // Give the new process a moment to be requested before exiting.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(800));
        std::process::exit(0);
    });

    std::process::Command::new("powershell")
        .args(["-ExecutionPolicy", "Bypass", "-Command", &ps])
        .spawn()
        .map_err(|e| format!("Failed to request elevation: {e}"))?;

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = logger::init();

    log::info!("MULA starting");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            open_config_dir,
            vsd_start,
            vsd_stop,
            vsd_is_running,
            vsd_get_logs,
            vsd_get_download_dir,
            vsd_set_download_dir,
            vsd_get_autostart,
            vsd_set_autostart,
            vsd_install_extension,
            list_physical_drives,
            get_drive_details,
            get_drive_smart,
            log_message,
            restart_as_admin,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
