use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};

mod elevated_helper;
mod error;
mod logger;
mod smart;
mod startup;
mod util;
mod vsd;
mod wallchanger;


#[derive(Serialize)]
pub struct AppInfo {
    version: String,
    config_path: String,
    platform: String,
    arch: String,
}

#[tauri::command]
fn get_app_info() -> AppInfo {
    let config_dir = util::mula_config_dir()
        .map(|p| p.to_string_lossy().to_string())
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
    let config_dir = util::mula_config_dir().ok_or("Could not determine config directory")?;

    std::fs::create_dir_all(&config_dir)
        .map_err(|e| format!("Failed to create config directory: {e}"))?;

    Ok(config_dir.to_string_lossy().to_string())
}

/// Holds a handle to the tray menu's "Start MULA with Windows" check item so its
/// checked state can be kept in sync with the System tab toggle, and vice versa.
struct AutostartMenuState(tauri::menu::CheckMenuItem<tauri::Wry>);

#[tauri::command]
fn get_autostart() -> Result<bool, String> {
    startup::is_enabled()
}

/// Applies "Start MULA with Windows" and keeps the tray item and the System tab in sync.
fn apply_autostart(app: &tauri::AppHandle, enabled: bool) -> Result<(), String> {
    use tauri::{Emitter, Manager};

    startup::set_enabled(enabled)?;
    if let Some(state) = app.try_state::<AutostartMenuState>() {
        let _ = state.0.set_checked(enabled);
    }
    let _ = app.emit("autostart-changed", enabled);
    Ok(())
}

#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    apply_autostart(&app, enabled)
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
    /// True when the disk holds the running Windows installation (it must never be formatted).
    is_system: bool,
}

fn holds_system_drive(drive_letters: &[String], system_letter: &str) -> bool {
    drive_letters.iter().any(|l| l.eq_ignore_ascii_case(system_letter))
}

/// The drive letter Windows is running from, e.g. "C:".
fn system_drive_letter() -> String {
    std::env::var("SystemDrive")
        .unwrap_or_else(|_| "C:".to_string())
        .trim_end_matches('\\')
        .to_uppercase()
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

    // Brand names written out as the first word.
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
        _ => "",
    };
    if !exact_vendor.is_empty() {
        return Some(exact_vendor.to_string());
    }

    // The leading letters of a model number.
    let vendor = match prefix.as_str() {
        "ST" | "STX" => "Seagate",
        "WD" | "WDC" | "WDS" => "Western Digital",
        "SK" | "HFS" | "HFM" => "SK hynix",
        "PNY" | "CS" => "PNY",
        "CT" => "Crucial",
        "KXG" | "XQ" => "Toshiba",
        "SD" => "SanDisk",
        "KC" | "SUV" | "SH" | "SKC" => "Kingston",
        "MT" => "Micron",
        "HTS" | "HUA" => "Hitachi",
        "HUS" | "HUH" | "HM" => "HGST",
        "SU" | "ASU" | "XPG" => "ADATA",
        "SP" | "SPCC" => "Silicon Power",
        "TS" => "Transcend",
        "NM" | "NQ" | "NS" => "Lexar",
        _ => {
            // Prefixes followed directly by more letters or digits ("SA400S37240G",
            // "MTFDDAK256TBN") never equal the leading letter run, so match those by prefix.
            const LONG_PREFIXES: &[(&str, &str)] = &[
                ("SA400", "Kingston"),
                ("MTFD", "Micron"),
                ("SDSS", "SanDisk"),
                ("THNS", "Toshiba"),
                ("SSDPE", "Intel"),
                ("CSSD", "Corsair"),
                ("T253", "Team"),
                ("SKHYNIX", "SK hynix"),
            ];
            return LONG_PREFIXES
                .iter()
                .find(|(prefix, _)| first_token.starts_with(prefix))
                .map(|(_, vendor)| vendor.to_string());
        }
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
    const UNITS: &[&str] = &["bps", "Kbps", "Mbps", "Gbps", "Tbps"];
    let mut size = bits_per_sec as f64;
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

/// PowerShell's ConvertTo-Json emits a single string for one-element arrays.
#[cfg(target_os = "windows")]
fn value_to_strings(v: Option<serde_json::Value>) -> Vec<String> {
    match v {
        Some(serde_json::Value::String(s)) => vec![s],
        Some(serde_json::Value::Array(arr)) => arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect(),
        _ => vec![],
    }
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
        let drive_type = media_type.clone();

        let connection_speed_text = match d.connection_speed {
            Some(bps) if bps > 0 => format_bitrate(bps),
            _ => infer_connection_speed(&bus_type, d.interface_type.as_deref().unwrap_or("")),
        };

        let drive_letters = value_to_strings(d.drive_letters);
        let is_system = holds_system_drive(&drive_letters, &system_drive_letter());

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
            drive_letters,
            mount_points: value_to_strings(d.mount_points),
            health_status: d.health_status.unwrap_or_default(),
            connection_speed: connection_speed_text,
            smart_capable: d.smart_capable.as_deref().unwrap_or("Unknown").to_string(),
            trim_capable: d.trim_capable.as_deref().unwrap_or("Unknown").to_string(),
            is_system,
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
    util::blocking(list_physical_drives_impl).await
}

fn find_drive(id: &str) -> Result<DriveInfo, String> {
    list_physical_drives_impl()?
        .into_iter()
        .find(|d| d.device_id == id)
        .ok_or_else(|| format!("Drive not found: {id}"))
}

/// Erases a drive and creates one NTFS partition (full format). Runs in its own elevated
/// PowerShell with a dedicated UAC prompt, and refuses the disk Windows is running from.
const FORMAT_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
function Write-Result([string]$text) {
    [IO.File]::WriteAllText($ResultPath, $text, (New-Object System.Text.UTF8Encoding $false))
}
try {
    $wmiDisk = Get-CimInstance -ClassName Win32_DiskDrive | Where-Object { $_.DeviceID -eq $DeviceId } | Select-Object -First 1
    if (-not $wmiDisk) { throw "Disk not found for device '$DeviceId'" }
    $number = [int]$wmiDisk.Index
    $disk = Get-Disk -Number $number
    if ($disk.IsSystem -or $disk.IsBoot) {
        throw "Refusing to format disk ${number}: it holds the running Windows installation."
    }
    $systemPart = Get-Partition -DriveLetter ($env:SystemDrive.TrimEnd(':')) -ErrorAction SilentlyContinue
    if ($systemPart -and $systemPart.DiskNumber -eq $number) {
        throw "Refusing to format disk ${number}: it holds the system drive $env:SystemDrive."
    }
    if ($disk.IsOffline) { Set-Disk -Number $number -IsOffline $false }
    if ($disk.IsReadOnly) { Set-Disk -Number $number -IsReadOnly $false }

    # Clear-Disk leaves the disk uninitialized (RAW), so it has to be initialized again
    # before a partition can be created.
    if ((Get-Disk -Number $number).PartitionStyle -ne 'RAW') {
        Clear-Disk -Number $number -RemoveData -RemoveOEM -Confirm:$false
    }
    Initialize-Disk -Number $number -PartitionStyle GPT
    $part = New-Partition -DiskNumber $number -UseMaximumSize -AssignDriveLetter
    $null = Format-Volume -Partition $part -FileSystem NTFS -NewFileSystemLabel 'MULA' -Full -Confirm:$false
    $letter = (Get-Partition -DiskNumber $number -PartitionNumber $part.PartitionNumber).DriveLetter
    if ($letter) {
        Write-Result "Formatted drive ${letter}: as NTFS (full format complete)"
    } else {
        Write-Result "Formatted disk $number as NTFS (full format complete)"
    }
    exit 0
} catch {
    Write-Result $_.Exception.Message
    exit 1
}
"#;

#[tauri::command]
async fn format_drive(id: String) -> Result<String, error::CommandError> {
    util::blocking(move || {
        let drive = find_drive(&id)?;
        if drive.is_system {
            return Err(format!(
                "Refusing to format {}: it holds the running Windows installation ({}).",
                drive.model,
                system_drive_letter()
            )
            .into());
        }

        let result = elevated_helper::run_elevated_script(FORMAT_SCRIPT, &[("DeviceId", &drive.device_id)]);
        match &result {
            Ok(msg) => log::info!("format_drive {}: {msg}", drive.device_id),
            Err(e) => log::error!("format_drive {} failed: {e}", drive.device_id),
        }
        result
    })
    .await
}

#[tauri::command]
async fn log_message(level: String, message: String) {
    logger::log_message(&level, &message);
}

/// Stops everything MULA started that would otherwise outlive it.
fn shutdown() {
    elevated_helper::stop_helper();
    vsd::stop_process();
}

/// Runs a blocking tray action off the UI thread and logs failures.
fn spawn_tray_action(name: &'static str, action: impl FnOnce() -> Result<(), String> + Send + 'static) {
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = action() {
            log::error!("Tray action '{name}' failed: {e}");
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = logger::init();

    log::info!("MULA starting");

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            open_config_dir,
            vsd::vsd_start,
            vsd::vsd_stop,
            vsd::vsd_is_running,
            vsd::vsd_get_logs,
            vsd::vsd_get_download_dir,
            vsd::vsd_set_download_dir,
            vsd::vsd_get_autostart,
            vsd::vsd_set_autostart,
            vsd::vsd_install_extension,
            list_physical_drives,
            smart::get_drive_smart,
            smart::run_drive_test,
            smart::get_drive_self_test_status,
            smart::save_smart_snapshot,
            smart::install_smartmontools,
            format_drive,
            log_message,
            wallchanger::wc_get_settings,
            wallchanger::wc_update_settings,
            wallchanger::wc_get_monitors,
            wallchanger::wc_change_now,
            wallchanger::wc_toggle_service,
            wallchanger::wc_get_status,
            wallchanger::wc_get_queue_preview,
            wallchanger::wc_get_monitor_preview,
            get_autostart,
            set_autostart,
        ])
        .on_window_event(|window, event| {
            // Closing the main window only hides it: MULA keeps running in the tray (the
            // wallpaper service, VSD server, ...). Use the tray's "Exit" to quit.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            use tauri::menu::{CheckMenuItem, Menu, MenuItem, SubmenuBuilder};
            use tauri::tray::TrayIconBuilder;
            use tauri::Manager;

            wallchanger::set_app_handle(app.handle().clone());

            let show = MenuItem::with_id(app, "show", "Show MULA", true, None::<&str>)?;

            let change = MenuItem::with_id(app, "change", "Change wallpaper", true, None::<&str>)?;
            let start = MenuItem::with_id(app, "start", "Start", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?;

            let current_max_level = wallchanger::settings::load()
                .map(|s| s.maximum_source_level)
                .unwrap_or(10);
            let mut max_level_items = Vec::new();
            for level in 1..=10 {
                let item = CheckMenuItem::with_id(
                    app,
                    format!("max_level_{level}"),
                    level.to_string(),
                    true,
                    level == current_max_level,
                    None::<&str>,
                )?;
                max_level_items.push(item);
            }
            let max_level_submenu = SubmenuBuilder::new(app, "Max source level")
                .items(&max_level_items.iter().map(|i| i as &dyn tauri::menu::IsMenuItem<_>).collect::<Vec<_>>())
                .build()?;
            app.manage(wallchanger::MaxLevelMenuState(
                (1..=10).zip(max_level_items.into_iter()).collect(),
            ));

            let wallchanger_submenu = SubmenuBuilder::new(app, "Wall Changer")
                .items(&[&change, &start, &stop, &max_level_submenu])
                .build()?;

            let autostart_enabled = startup::is_enabled().unwrap_or(false);
            let autostart = CheckMenuItem::with_id(
                app,
                "autostart",
                "Start MULA with Windows",
                true,
                autostart_enabled,
                None::<&str>,
            )?;

            let quit = MenuItem::with_id(app, "quit", "Exit", true, None::<&str>)?;

            let menu = Menu::with_items(app, &[&show, &wallchanger_submenu, &autostart, &quit])?;

            let autostart_menu_item = autostart.clone();
            app.manage(AutostartMenuState(autostart.clone()));

            TrayIconBuilder::with_id("mula-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("MULA")
                .menu(&menu)
                .on_menu_event(move |app, event| match event.id().as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                    // Wallpaper work blocks (disk, network, fades); keep it off the UI thread.
                    "change" => spawn_tray_action("change wallpaper", || {
                        wallchanger::service::apply(false).map(|_| ())
                    }),
                    "start" => spawn_tray_action("start service", || wallchanger::service::set_running(true)),
                    "stop" => spawn_tray_action("stop service", || wallchanger::service::set_running(false)),
                    "autostart" => {
                        let enabled = autostart_menu_item.is_checked().unwrap_or(false);
                        if let Err(e) = apply_autostart(app, enabled) {
                            log::error!("Failed to update autostart setting: {e}");
                            let _ = autostart_menu_item.set_checked(!enabled);
                        }
                    }
                    id if id.starts_with("max_level_") => {
                        if let Ok(level) = id.trim_start_matches("max_level_").parse::<i32>() {
                            let result = wallchanger::settings::update(|s| s.maximum_source_level = level);
                            match result {
                                Ok(versioned) => wallchanger::on_settings_changed(app, &versioned),
                                Err(e) => log::error!("Failed to update max source level: {e}"),
                            }
                        }
                    }
                    "quit" => {
                        shutdown();
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            // Start the VSD server if the user asked for it to start automatically.
            vsd::autostart(app.handle().clone());

            // Resume the wallpaper service if it was running.
            if wallchanger::settings::service_running() {
                let _ = wallchanger::service::start();
            }

            // When launched from the Windows startup entry, start hidden in the tray.
            if std::env::args().any(|arg| arg == startup::AUTOSTART_ARG) {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|_app, event| {
        if let tauri::RunEvent::Exit = event {
            shutdown();
        }
    });
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_is_parsed_from_pnp_device_ids() {
        assert_eq!(
            parse_vendor_from_pnp(r"SCSI\DISK&VEN_WDC&PROD_WD3003FZEX-00Z4S\4&1A2B&0&000000").as_deref(),
            Some("WDC")
        );
        assert_eq!(parse_vendor_from_pnp(r"SCSI\DISK&VEN_SAMSUNG\X").as_deref(), Some("SAMSUNG"));
        assert_eq!(parse_vendor_from_pnp(r"SCSI\DISK&VEN_&PROD_X"), None);
        assert_eq!(parse_vendor_from_pnp(r"USBSTOR\DISK&PROD_X"), None);
        assert_eq!(parse_vendor_from_pnp(""), None);
    }

    #[test]
    fn bus_and_controller_names_are_not_vendors() {
        for not_vendor in ["NVMe", "scsi", " USB ", "Microsoft", "VMware", "", "   "] {
            assert!(!is_real_vendor(not_vendor), "{not_vendor:?} should not count as a vendor");
        }
        for vendor in ["WDC", "Samsung", "Kingston"] {
            assert!(is_real_vendor(vendor), "{vendor:?} should count as a vendor");
        }
    }

    #[test]
    fn vendor_is_guessed_from_common_model_strings() {
        let cases = [
            ("Samsung SSD 970 EVO Plus 1TB", Some("Samsung")),
            ("INTEL SSDPEKNW512G8", Some("Intel")),
            ("KINGSTON SA400S37240G", Some("Kingston")),
            ("ST2000DM008-2FR102", Some("Seagate")),
            ("WDC WD3003FZEX-00Z4SA0", Some("Western Digital")),
            ("WDS500G2B0A-00SM50", Some("Western Digital")),
            ("CT1000MX500SSD1", Some("Crucial")),
            ("HFS512GD9TNG-L2A0A", Some("SK hynix")),
            ("TS256GSSD230S", Some("Transcend")),
            ("HUS724040ALE640", Some("HGST")),
            ("PNY CS900 240GB", Some("PNY")),
            ("Generic Flash Disk", None),
            ("", None),
        ];
        for (model, expected) in cases {
            assert_eq!(guess_vendor_from_model(model).as_deref(), expected, "model {model:?}");
        }
    }

    #[test]
    fn vendor_is_guessed_from_model_prefixes_followed_by_more_letters() {
        assert_eq!(guess_vendor_from_model("SA400S37240G").as_deref(), Some("Kingston"));
        assert_eq!(guess_vendor_from_model("MTFDDAK256TBN").as_deref(), Some("Micron"));
        assert_eq!(guess_vendor_from_model("SDSSDA-240G").as_deref(), Some("SanDisk"));
        assert_eq!(guess_vendor_from_model("THNSN5256GPUK").as_deref(), Some("Toshiba"));
    }

    #[test]
    fn byte_sizes_are_formatted_with_binary_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023.00 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(500 * 1024 * 1024 * 1024), "500.00 GB");
        assert_eq!(format_bytes(1_500_000_000_000), "1.36 TB");
        assert_eq!(format_bytes(u64::MAX), "16384.00 PB");
    }

    #[test]
    fn bitrates_are_formatted_with_decimal_units() {
        assert_eq!(format_bitrate(0), "Unknown");
        assert_eq!(format_bitrate(999), "999.00 bps");
        assert_eq!(format_bitrate(6_000_000_000), "6.00 Gbps");
        assert_eq!(format_bitrate(1_500_000), "1.50 Mbps");
    }

    #[test]
    fn connection_speed_is_inferred_from_bus_or_interface() {
        assert_eq!(infer_connection_speed("NVMe", "SCSI"), "PCIe (varies by generation)");
        assert_eq!(infer_connection_speed("SATA", ""), "Up to 6 Gbps (SATA 6 Gb/s)");
        assert_eq!(infer_connection_speed("SAS", ""), "Up to 12 Gbps (SAS 12 Gb/s)");
        assert_eq!(infer_connection_speed("Unknown", "USB"), "USB (varies by port)");
        assert_eq!(infer_connection_speed("", "IDE"), "Up to 133 MB/s (PATA)");
        assert_eq!(infer_connection_speed("", "SCSI"), "SCSI (varies by generation)");
        assert_eq!(infer_connection_speed("", ""), "Unknown");
    }








    #[test]
    fn system_disk_is_detected_from_its_drive_letters() {
        let letters = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(holds_system_drive(&letters(&["D:", "C:"]), "C:"));
        assert!(holds_system_drive(&letters(&["c:"]), "C:"));
        assert!(!holds_system_drive(&letters(&["D:"]), "C:"));
        assert!(!holds_system_drive(&[], "C:"));
    }

    #[test]
    fn system_drive_letter_has_no_trailing_backslash() {
        let letter = system_drive_letter();
        assert!(letter.ends_with(':'), "{letter}");
        assert_eq!(letter, letter.to_uppercase());
    }






    #[test]
    fn format_script_targets_only_the_requested_disk_and_guards_the_system_disk() {
        assert!(FORMAT_SCRIPT.contains("$disk.IsSystem -or $disk.IsBoot"));
        assert!(FORMAT_SCRIPT.contains("Initialize-Disk"), "Clear-Disk leaves the disk RAW");
        // The device id arrives as the $DeviceId variable (a literal, see ps_script).
        assert!(FORMAT_SCRIPT.contains("$_.DeviceID -eq $DeviceId"));
        assert!(!FORMAT_SCRIPT.contains("__"), "no text placeholders left");
    }
}
