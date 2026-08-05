use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static ELEVATED_HELPER: Mutex<Option<ElevatedHelper>> = Mutex::new(None);

struct ElevatedHelper {
    cmd_dir: PathBuf,
}

#[derive(Serialize)]
struct ElevatedCommand {
    exe: String,
    args: Vec<String>,
    out: String,
    err: String,
    done: String,
    exit: bool,
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn ps_path(path: &Path) -> String {
    path.to_string_lossy().replace("\\", "/")
}

fn start_helper() -> Result<ElevatedHelper, String> {
    let pid = std::process::id();
    let ts = now_nanos();
    let cmd_dir = std::env::temp_dir().join(format!("mula_smartd_{pid}_{ts}"));
    std::fs::create_dir_all(&cmd_dir)
        .map_err(|e| format!("Failed to create helper directory: {e}"))?;

    let script_path = cmd_dir.join("mula_smartd.ps1");
    let script = r#"param([string]$Dir)
$ready = Join-Path $Dir "ready.txt"
$null = New-Item -ItemType File -Path $ready -Force
while ($true) {
    $cmdFiles = Get-ChildItem -Path $Dir -Filter "cmd_*.json" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime
    if ($cmdFiles) {
        $cmdFile = $cmdFiles[0].FullName
        $json = Get-Content $cmdFile -Raw -Encoding utf8
        $cmd = $json | ConvertFrom-Json
        Remove-Item $cmdFile -ErrorAction SilentlyContinue
        $doneFile = Join-Path $Dir $cmd.done
        if ($cmd.exit) { $null = New-Item -ItemType File -Path $doneFile -Force; break }
        if (Test-Path $doneFile) { Remove-Item $doneFile -ErrorAction SilentlyContinue }
        $argList = @($cmd.args | ForEach-Object { "$_" })
        Start-Process -FilePath $cmd.exe -ArgumentList $argList -Wait -NoNewWindow -RedirectStandardOutput $cmd.out -RedirectStandardError $cmd.err
        $null = New-Item -ItemType File -Path $doneFile -Force
    }
    Start-Sleep -Milliseconds 100
}
"#;
    std::fs::write(&script_path, script)
        .map_err(|e| format!("Failed to write helper script: {e}"))?;

    let script_ps = ps_path(&script_path);
    let dir_ps = ps_path(&cmd_dir);
    let ps = format!(
        r#"Start-Process -FilePath "powershell" -ArgumentList '-WindowStyle','Hidden','-ExecutionPolicy','Bypass','-File','{}','-Dir','{}' -Verb runAs -WindowStyle Hidden"#,
        script_ps, dir_ps
    );

    log::info!("Starting elevated smartctl helper: {}", ps);

    Command::new("powershell")
        .args(["-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-Command", &ps])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start elevated helper launcher: {e}"))?;

    let ready_file = cmd_dir.join("ready.txt");
    for _ in 0..300 {
        if ready_file.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if !ready_file.exists() {
        return Err("Elevated helper did not become ready".to_string());
    }

    log::info!("Elevated smartctl helper is ready");
    Ok(ElevatedHelper { cmd_dir })
}

#[allow(dead_code)]
pub fn stop_helper() {
    let helper = {
        let mut guard = match ELEVATED_HELPER.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        guard.take()
    };

    if let Some(h) = helper {
        let uuid = format!("{}_{}", std::process::id(), now_nanos());
        let cmd_file = h.cmd_dir.join(format!("cmd_{uuid}.json"));
        let done_file = h.cmd_dir.join(format!("done_{uuid}.txt"));
        let exit_cmd = ElevatedCommand {
            exe: String::new(),
            args: vec![],
            out: String::new(),
            err: String::new(),
            done: done_file.file_name().unwrap().to_string_lossy().to_string(),
            exit: true,
        };
        if let Ok(json) = serde_json::to_string(&exit_cmd) {
            let _ = std::fs::write(&cmd_file, json);
        }
        for _ in 0..50 {
            if done_file.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::remove_dir_all(&h.cmd_dir);
    }
}

pub fn run_elevated(smartctl: &Path, args: &[&str]) -> Result<String, String> {
    let mut guard = ELEVATED_HELPER
        .lock()
        .map_err(|_| "Elevated helper lock poisoned")?;

    if guard.is_none() {
        let helper = start_helper().map_err(|e| {
            log::error!("Failed to start elevated helper: {e}");
            e
        })?;
        *guard = Some(helper);
    }

    let helper = guard.as_ref().unwrap();
    let uuid = format!("{}_{}", std::process::id(), now_nanos());
    let cmd_file = helper.cmd_dir.join(format!("cmd_{uuid}.json"));
    let done_file = helper.cmd_dir.join(format!("done_{uuid}.txt"));
    let out_file = helper.cmd_dir.join(format!("out_{uuid}.txt"));
    let err_file = helper.cmd_dir.join(format!("err_{uuid}.txt"));

    let cmd = ElevatedCommand {
        exe: ps_path(smartctl),
        args: args.iter().map(|a| a.to_string()).collect(),
        out: ps_path(&out_file),
        err: ps_path(&err_file),
        done: done_file.file_name().unwrap().to_string_lossy().to_string(),
        exit: false,
    };

    let json = serde_json::to_string(&cmd)
        .map_err(|e| format!("Failed to serialize helper command: {e}"))?;
    let tmp_file = cmd_file.with_extension("tmp");
    std::fs::write(&tmp_file, json)
        .map_err(|e| format!("Failed to write helper command file: {e}"))?;
    std::fs::rename(&tmp_file, &cmd_file)
        .map_err(|e| format!("Failed to rename helper command file: {e}"))?;

    log::info!("Sent elevated smartctl command: {}", cmd_file.display());

    for _ in 0..600 {
        if done_file.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if !done_file.exists() {
        log::error!("Elevated helper did not respond for {}", cmd_file.display());
        return Err("Elevated helper did not respond".to_string());
    }

    let _ = std::fs::remove_file(&done_file);
    let _ = std::fs::remove_file(&cmd_file);

    let stdout_bytes = std::fs::read(&out_file)
        .map_err(|e| format!("Failed to read helper stdout '{}': {}", out_file.display(), e))?;
    let stderr_bytes = std::fs::read(&err_file)
        .map_err(|e| format!("Failed to read helper stderr '{}': {}", err_file.display(), e))?;
    let _ = std::fs::remove_file(&out_file);
    let _ = std::fs::remove_file(&err_file);

    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();

    log::info!(
        "Elevated helper finished: stdout_len={}, stderr_len={}",
        stdout.len(),
        stderr.len()
    );

    if !stdout.trim().is_empty() {
        Ok(stdout)
    } else if !stderr.trim().is_empty() {
        Err(format!("smartctl error: {}", stderr.trim()))
    } else {
        Err("smartctl returned no output".to_string())
    }
}
