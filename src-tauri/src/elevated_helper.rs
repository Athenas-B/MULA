//! Runs commands with administrator rights.
//!
//! Two mechanisms are provided:
//!
//! * A long-lived elevated **smartctl helper** ([`run_smartctl_elevated`]), so polling self-test
//!   progress does not raise a UAC prompt every few seconds. It talks to MULA through command /
//!   response files in a temp directory.
//! * A **one-shot elevated script** ([`run_elevated_script`]) that raises its own UAC prompt and
//!   is used for destructive operations such as formatting a drive. Those never go through the
//!   long-lived helper.
//!
//! Security model: the temp directory is writable by every process running as the same user, so
//! anything placed there must be treated as untrusted by the elevated side. Therefore:
//! - the elevated process is started with a tiny inline stub (`-EncodedCommand`) that reads the
//!   real script into memory and only runs it if its SHA-256 matches the hash baked into the stub,
//!   so the script file cannot be swapped before elevation;
//! - the helper only ever runs the smartctl executable fixed at launch, which must live outside
//!   the user's profile, and only with an allowlist of read-only / self-test argument shapes;
//! - every command must carry a random per-session token;
//! - the helper names its own response files (never paths taken from a command), creates them
//!   with `CreateNew`, and stops if its directory has been replaced by a reparse point;
//! - the helper exits by itself once the MULA process that started it has gone away.

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static ELEVATED_HELPER: Mutex<Option<ElevatedHelper>> = Mutex::new(None);

struct ElevatedHelper {
    cmd_dir: PathBuf,
    exe: PathBuf,
    token: String,
}

#[derive(Serialize)]
struct HelperCommand<'a> {
    token: &'a str,
    args: Vec<String>,
    exit: bool,
}

#[derive(Deserialize)]
struct HelperResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    output: String,
    #[serde(default)]
    error: String,
}

/// The long-lived helper. `__DIR__`, `__EXE__`, `__TOKEN__` and `__PARENT_PID__` are substituted
/// before the script is written to disk; its hash is then pinned in the elevation stub.
const HELPER_SCRIPT: &str = r#"
$ErrorActionPreference = 'Continue'
$Dir = '__DIR__'
$Exe = '__EXE__'
$Token = '__TOKEN__'
$ParentPid = __PARENT_PID__
$utf8 = New-Object System.Text.UTF8Encoding $false
$idRe = '^[0-9A-Za-z_]{1,64}$'
$typeRe = '^[A-Za-z0-9,+_-]{1,32}$'
$deviceRe = '^/dev/[A-Za-z0-9,]{1,16}$'
$allowedHeads = @('-i', '-a', '-t short', '-t long', '-t conveyance', '-t offline', '-l selftest')

function Test-DirIsSafe {
    $item = Get-Item -LiteralPath $Dir -Force -ErrorAction SilentlyContinue
    return ($item -and -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint))
}

function Test-Args([string[]]$a) {
    $n = $a.Count
    if ($n -eq 1) { return $a[0] -ceq '--scan' }
    if ($n -lt 4) { return $false }
    if ($a[$n - 3] -cne '-d' -or $a[$n - 2] -notmatch $typeRe -or $a[$n - 1] -notmatch $deviceRe) { return $false }
    $head = ($a[0..($n - 4)]) -join ' '
    return $allowedHeads -ccontains $head
}

function Write-Response([string]$id, $res) {
    if (-not (Test-DirIsSafe)) { exit 3 }
    $tmp = Join-Path $Dir ('res_' + $id + '.tmp')
    $final = Join-Path $Dir ('res_' + $id + '.json')
    Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    try {
        $bytes = $utf8.GetBytes(($res | ConvertTo-Json -Compress))
        $fs = [IO.File]::Open($tmp, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
        try { $fs.Write($bytes, 0, $bytes.Length) } finally { $fs.Dispose() }
        Move-Item -LiteralPath $tmp -Destination $final -Force
    } catch {}
}

if (-not (Test-DirIsSafe)) { exit 3 }
[IO.File]::WriteAllText((Join-Path $Dir 'ready'), '', $utf8)

while ($true) {
    if (-not (Get-Process -Id $ParentPid -ErrorAction SilentlyContinue)) { break }
    $files = Get-ChildItem -LiteralPath $Dir -Filter 'cmd_*.json' -File -ErrorAction SilentlyContinue | Sort-Object LastWriteTime
    foreach ($f in $files) {
        $id = $f.BaseName.Substring(4)
        $raw = $null
        try { $raw = [IO.File]::ReadAllText($f.FullName, $utf8) } catch { continue }
        Remove-Item -LiteralPath $f.FullName -Force -ErrorAction SilentlyContinue
        if ($id -notmatch $idRe) { continue }

        $res = @{ ok = $false; output = ''; error = '' }
        $exitAfter = $false
        try {
            $cmd = $raw | ConvertFrom-Json
            if ($cmd.token -cne $Token) {
                $res.error = 'invalid token'
            } elseif ($cmd.exit) {
                $res.ok = $true
                $exitAfter = $true
            } else {
                $a = @($cmd.args | ForEach-Object { [string]$_ })
                if (-not (Test-Args $a)) {
                    $res.error = 'command not allowed: smartctl ' + ($a -join ' ')
                } else {
                    $out = & $Exe @a 2>&1 | ForEach-Object { "$_" }
                    $res.output = ($out -join "`r`n")
                    $res.ok = $true
                }
            }
        } catch {
            $res.error = $_.Exception.Message
        }
        Write-Response $id $res
        if ($exitAfter) { exit 0 }
    }
    Start-Sleep -Milliseconds 100
}
"#;

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn random_token() -> String {
    use rand::Rng;
    let bytes: [u8; 24] = rand::thread_rng().gen();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Quotes a value as a PowerShell single-quoted string literal.
pub fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Encodes a script for `powershell -EncodedCommand` (base64 of UTF-16LE).
pub fn encode_ps(script: &str) -> String {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02X}")).collect()
}

fn new_private_dir(prefix: &str) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), now_nanos()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create helper directory: {e}"))?;
    Ok(dir)
}

/// Writes `script` to `dir` and returns an inline stub that runs it only if its SHA-256 matches.
fn write_script_with_stub(dir: &Path, script: &str) -> Result<String, String> {
    let bytes = script.as_bytes();
    let script_path = dir.join("script.ps1");
    std::fs::write(&script_path, bytes).map_err(|e| format!("Failed to write elevated script: {e}"))?;

    Ok(format!(
        "$b = [IO.File]::ReadAllBytes({path}); \
         $h = ([BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($b))) -replace '-', ''; \
         if ($h -ne '{hash}') {{ exit 87 }}; \
         & ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString($b)))",
        path = ps_quote(&script_path.to_string_lossy()),
        hash = sha256_hex(bytes),
    ))
}

/// Starts `stub` in a new PowerShell process, elevated through UAC when `elevate` is set.
/// With `wait`, blocks until it finishes and returns its exit code.
fn launch_stub(stub: &str, elevate: bool, wait: bool) -> Result<i32, String> {
    let verb = if elevate { "-Verb RunAs " } else { "" };
    let wait_flags = if wait { "-Wait -PassThru " } else { "" };
    let launcher = format!(
        "$ErrorActionPreference = 'Stop';          try {{ $p = Start-Process -FilePath 'powershell.exe' {verb}{wait_flags}-WindowStyle Hidden          -ArgumentList '-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-WindowStyle','Hidden','-EncodedCommand','{encoded}' }}          catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 9001 }};          {exit}",
        encoded = encode_ps(stub),
        exit = if wait { "exit $p.ExitCode" } else { "exit 0" },
    );

    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-EncodedCommand", &encode_ps(&launcher)])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to start PowerShell: {e}"))?;

    let code = output.status.code().unwrap_or(-1);
    if code == 9001 {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.to_lowercase().contains("canceled by the user") {
            return Err("Administrator rights were declined".to_string());
        }
        return Err(format!("Failed to start elevated process: {stderr}"));
    }
    Ok(code)
}

/// Refuses executables the current user could replace, since the helper runs them elevated.
fn check_exe_is_protected(exe: &Path) -> Result<(), String> {
    if !exe.is_absolute() {
        return Err(format!("smartctl path must be absolute to run elevated: {}", exe.display()));
    }
    let exe_lower = exe.to_string_lossy().to_lowercase();
    let user_dirs = [dirs::home_dir(), Some(std::env::temp_dir())];
    for dir in user_dirs.into_iter().flatten() {
        let dir_lower = dir.to_string_lossy().to_lowercase();
        if !dir_lower.is_empty() && exe_lower.starts_with(&dir_lower) {
            return Err(format!(
                "Refusing to run {} with administrator rights because it lives in a user-writable folder. \
                 Install smartmontools to Program Files instead.",
                exe.display()
            ));
        }
    }
    Ok(())
}

fn start_helper(exe: &Path, elevate: bool) -> Result<ElevatedHelper, String> {
    check_exe_is_protected(exe)?;

    let cmd_dir = new_private_dir("mula_smartd")?;
    let token = random_token();
    let script = HELPER_SCRIPT
        .replace("__DIR__", &cmd_dir.to_string_lossy().replace('\'', "''"))
        .replace("__EXE__", &exe.to_string_lossy().replace('\'', "''"))
        .replace("__TOKEN__", &token)
        .replace("__PARENT_PID__", &std::process::id().to_string());
    let stub = write_script_with_stub(&cmd_dir, &script)?;

    log::info!("Starting elevated smartctl helper in {}", cmd_dir.display());
    launch_stub(&stub, elevate, false)?;

    let ready_file = cmd_dir.join("ready");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready_file.exists() {
        if Instant::now() >= deadline {
            let _ = std::fs::remove_dir_all(&cmd_dir);
            return Err("Elevated helper did not become ready (was the UAC prompt declined?)".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    log::info!("Elevated smartctl helper is ready");
    Ok(ElevatedHelper { cmd_dir, exe: exe.to_path_buf(), token })
}

fn send_command(helper: &ElevatedHelper, args: &[&str], exit: bool, timeout: Duration) -> Result<HelperResponse, String> {
    let id = format!("{}_{}", std::process::id(), now_nanos());
    let cmd_file = helper.cmd_dir.join(format!("cmd_{id}.json"));
    let res_file = helper.cmd_dir.join(format!("res_{id}.json"));

    let cmd = HelperCommand {
        token: &helper.token,
        args: args.iter().map(|a| a.to_string()).collect(),
        exit,
    };
    let json = serde_json::to_string(&cmd).map_err(|e| format!("Failed to serialize helper command: {e}"))?;
    let tmp_file = cmd_file.with_extension("tmp");
    std::fs::write(&tmp_file, json).map_err(|e| format!("Failed to write helper command file: {e}"))?;
    std::fs::rename(&tmp_file, &cmd_file).map_err(|e| format!("Failed to rename helper command file: {e}"))?;

    let start = Instant::now();
    while !res_file.exists() {
        if start.elapsed() >= timeout {
            let _ = std::fs::remove_file(&cmd_file);
            return Err("Elevated helper did not respond".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let bytes = std::fs::read(&res_file).map_err(|e| format!("Failed to read helper response: {e}"))?;
    let _ = std::fs::remove_file(&res_file);
    serde_json::from_slice(&bytes).map_err(|e| format!("Failed to parse helper response: {e}"))
}

pub fn stop_helper() {
    let helper = match ELEVATED_HELPER.lock() {
        Ok(mut guard) => guard.take(),
        Err(_) => return,
    };

    if let Some(h) = helper {
        let _ = send_command(&h, &[], true, Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&h.cmd_dir);
    }
}

fn run_smartctl_with_helper(exe: &Path, args: &[&str], elevate: bool) -> Result<String, String> {
    let mut guard = ELEVATED_HELPER.lock().map_err(|_| "Elevated helper lock poisoned")?;

    // A helper is bound to one executable; restart it if smartctl moved, or if it died.
    let reusable = guard.as_ref().is_some_and(|h| h.exe == exe && h.cmd_dir.join("ready").exists());
    if !reusable {
        if let Some(old) = guard.take() {
            let _ = std::fs::remove_dir_all(&old.cmd_dir);
        }
        *guard = Some(start_helper(exe, elevate).inspect_err(|e| log::error!("Failed to start elevated helper: {e}"))?);
    }

    let helper = guard.as_ref().unwrap();
    let response = match send_command(helper, args, false, Duration::from_secs(60)) {
        Ok(r) => r,
        Err(e) => {
            // Most likely the helper exited; drop it so the next call starts a fresh one.
            if let Some(old) = guard.take() {
                let _ = std::fs::remove_dir_all(&old.cmd_dir);
            }
            return Err(e);
        }
    };

    if !response.ok {
        return Err(format!("Elevated helper refused the command: {}", response.error));
    }
    if response.output.trim().is_empty() {
        return Err("command returned no output".to_string());
    }
    Ok(response.output)
}

/// Runs smartctl with administrator rights through the long-lived helper.
pub fn run_smartctl_elevated(exe: &Path, args: &[&str]) -> Result<String, String> {
    run_smartctl_with_helper(exe, args, true)
}

/// Runs `script` once in its own elevated PowerShell (one UAC prompt), waiting for it to finish.
///
/// The script receives the path of a result file via the `__RESULT_PATH__` placeholder, should
/// write a human-readable message to it, and should exit 0 on success. Returns that message.
pub fn run_elevated_script(script: &str) -> Result<String, String> {
    run_script(script, true)
}

fn run_script(script: &str, elevate: bool) -> Result<String, String> {
    let dir = new_private_dir("mula_elevated")?;
    let result_path = dir.join("result.txt");
    let script = script.replace("__RESULT_PATH__", &result_path.to_string_lossy().replace('\'', "''"));

    let outcome = write_script_with_stub(&dir, &script).and_then(|stub| launch_stub(&stub, elevate, true));
    let message = std::fs::read(&result_path)
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);

    match outcome? {
        0 => Ok(message),
        87 => Err("Elevated script failed its integrity check and was not run".to_string()),
        code if message.is_empty() => Err(format!("Elevated script failed (exit code {code})")),
        _ => Err(message),
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    fn where_exe() -> PathBuf {
        PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into()))
            .join("System32")
            .join("where.exe")
    }

    /// Exercises the real helper protocol without UAC by running the helper unelevated, with
    /// `where.exe` standing in for smartctl.
    #[test]
    fn helper_runs_allowlisted_commands_and_rejects_others() {
        let exe = where_exe();
        let helper = start_helper(&exe, false).expect("helper starts");

        let allowed = send_command(&helper, &["--scan"], false, Duration::from_secs(30)).unwrap();
        assert!(allowed.ok, "allowlisted command should run: {}", allowed.error);
        assert!(!allowed.output.is_empty());

        let shapes: &[&[&str]] = &[
            &["/c", "calc.exe"],
            &["-a", "-d", "sat", r"C:\Windows\notepad.exe"],
            &["-a", "-d", "sat & calc", "/dev/sda"],
            &["-s", "on", "-d", "sat", "/dev/sda"],
            &["-d", "sat", "/dev/sda"],
        ];
        for args in shapes {
            let res = send_command(&helper, args, false, Duration::from_secs(30)).unwrap();
            assert!(!res.ok, "{args:?} must be rejected");
            assert!(res.error.contains("not allowed"), "{args:?}: {}", res.error);
        }

        let tampered = ElevatedHelper { cmd_dir: helper.cmd_dir.clone(), exe: exe.clone(), token: "wrong".into() };
        let res = send_command(&tampered, &["--scan"], false, Duration::from_secs(30)).unwrap();
        assert!(!res.ok);
        assert_eq!(res.error, "invalid token");

        let bye = send_command(&helper, &[], true, Duration::from_secs(30)).unwrap();
        assert!(bye.ok);
        let _ = std::fs::remove_dir_all(&helper.cmd_dir);
    }

    #[test]
    fn helper_refuses_executables_in_user_folders() {
        let exe = std::env::temp_dir().join("smartctl.exe");
        assert!(start_helper(&exe, false).is_err());
        assert!(start_helper(Path::new("smartctl"), false).is_err());
    }

    #[test]
    fn one_shot_script_reports_message_and_exit_code() {
        let ok = run_script(
            "[IO.File]::WriteAllText('__RESULT_PATH__', 'it''s done'); exit 0",
            false,
        );
        assert_eq!(ok.unwrap(), "it's done");

        let failed = run_script("[IO.File]::WriteAllText('__RESULT_PATH__', 'boom'); exit 1", false);
        assert_eq!(failed.unwrap_err(), "boom");
    }

    #[test]
    fn stub_refuses_a_script_swapped_after_hashing() {
        let dir = new_private_dir("mula_elevated_test").unwrap();
        let marker = dir.join("ran.txt");
        let stub = write_script_with_stub(&dir, "exit 0").unwrap();
        let evil = format!("[IO.File]::WriteAllText({}, 'pwned'); exit 0", ps_quote(&marker.to_string_lossy()));
        std::fs::write(dir.join("script.ps1"), evil).unwrap();

        assert_eq!(launch_stub(&stub, false, true).unwrap(), 87);
        assert!(!marker.exists(), "tampered script must not run");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
