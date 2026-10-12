//! Runs commands with administrator rights.
//!
//! Two mechanisms are provided:
//!
//! * A long-lived elevated **smartctl helper** ([`run_smartctl_elevated`]), so polling self-test
//!   progress does not raise a UAC prompt every few seconds. It talks to MULA through command /
//!   response files in a temp directory; both sides sleep on directory change notifications
//!   instead of polling (the helper a `FileSystemWatcher`, MULA [`wait_for_file`]).
//!
//! Files rather than a named pipe: an elevated process started through UAC can't inherit
//! handles, so a pipe would need a name chosen up front, which another process of the same user
//! could read from the script and claim first while the UAC prompt is open (pipe squatting),
//! receiving the token and feeding MULA fake SMART data.
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

use crate::error::CommandError;
use base64::Engine;
use serde::{Deserialize, Serialize};
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

/// The long-lived helper. `$Dir`, `$Exe`, `$Token` and `$ParentPid` are defined by
/// [`ps_script`] before the script is written to disk; its hash is then pinned in the stub.
const HELPER_SCRIPT: &str = r#"
$ErrorActionPreference = 'Continue'
$utf8 = New-Object System.Text.UTF8Encoding $false
$idRe = '^[0-9A-Za-z_]{1,64}$'
$typeRe = '^[A-Za-z0-9,+_-]{1,32}$'
$deviceRe = '^/dev/[A-Za-z0-9,]{1,16}$'
$allowedHeads = @('-i', '-a', '-t short', '-t long', '-t conveyance', '-t offline', '-l selftest', '-c -l selftest')

function Test-DirIsSafe {
    $item = Get-Item -LiteralPath $Dir -Force -ErrorAction SilentlyContinue
    return ($item -and -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint))
}

function Test-Args([string[]]$a) {
    # JSON output (with the usual text included) may be requested in front of any command.
    if ($a.Count -gt 0 -and $a[0] -ceq '--json=o') { $a = @($a | Select-Object -Skip 1) }
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

# New command files wake the loop through a watcher. Its events are only a wake-up signal: the
# loop always lists and validates the folder itself, so a missed or spurious event costs at most
# a rescan. Events raised while commands run stay queued, so none is lost between scans.
$watcher = New-Object IO.FileSystemWatcher -ArgumentList $Dir, 'cmd_*.json'
$watcher.NotifyFilter = [IO.NotifyFilters]::FileName
$null = Register-ObjectEvent -InputObject $watcher -EventName Created -SourceIdentifier 'MulaCmdCreated'
$null = Register-ObjectEvent -InputObject $watcher -EventName Renamed -SourceIdentifier 'MulaCmdRenamed'
$watcher.EnableRaisingEvents = $true

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
    # Sleep until a command file appears, or 2 s pass so the helper notices MULA exiting.
    $null = Wait-Event -Timeout 2
    Get-Event | Remove-Event
}
"#;

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// Quotes a value as a PowerShell single-quoted string literal (never interpolated).
fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Builds a script from `template`, defining each `(name, value)` as a PowerShell variable
/// ahead of it. Values are single-quoted literals, so they can never run as code.
fn ps_script(template: &str, vars: &[(&str, &str)]) -> String {
    let mut script: String = vars.iter().map(|(name, value)| format!("${name} = {}\n", ps_quote(value))).collect();
    script.push_str(template);
    script
}

/// Encodes a script for `powershell -EncodedCommand` (base64 of UTF-16LE).
fn encode_ps(script: &str) -> String {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

/// Re-check interval when change notifications are unavailable or fail.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Waits until `file` exists in `dir` (created or renamed into it), up to `timeout`; returns
/// whether it appeared. Sleeps on a directory change notification rather than polling.
#[cfg(target_os = "windows")]
fn wait_for_file(dir: &Path, file: &Path, timeout: Duration) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::Storage::FileSystem::{
        FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification, FILE_NOTIFY_CHANGE_FILE_NAME,
    };
    use windows::Win32::System::Threading::WaitForSingleObject;

    let deadline = Instant::now() + timeout;
    // Registered before the first check, so a file appearing in between still signals it.
    let Ok(handle) = (unsafe { FindFirstChangeNotificationW(&HSTRING::from(dir), false, FILE_NOTIFY_CHANGE_FILE_NAME) }) else {
        return poll_for_file(file, deadline);
    };
    let found = loop {
        if file.exists() {
            break true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break false;
        }
        let wait_ms = remaining.as_millis().min(u32::MAX as u128) as u32;
        match unsafe { WaitForSingleObject(handle, wait_ms) } {
            WAIT_OBJECT_0 => {
                // Re-arm for the next change. If that fails (e.g. the folder was deleted) the
                // handle stays signaled, so throttle the loop into plain polling.
                if unsafe { FindNextChangeNotification(handle) }.is_err() {
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
            WAIT_TIMEOUT => {}
            _ => std::thread::sleep(POLL_INTERVAL),
        }
    };
    let _ = unsafe { FindCloseChangeNotification(handle) };
    found
}

#[cfg(not(target_os = "windows"))]
fn wait_for_file(_dir: &Path, file: &Path, timeout: Duration) -> bool {
    poll_for_file(file, Instant::now() + timeout)
}

fn poll_for_file(file: &Path, deadline: Instant) -> bool {
    loop {
        if file.exists() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
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
        hash = crate::util::sha256_hex(bytes),
    ))
}

/// Launcher exit codes for "Start-Process failed" and "the UAC prompt was declined".
const EXIT_START_FAILED: i32 = 9001;
const EXIT_UAC_DECLINED: i32 = 9002;

/// Starts `stub` in a new PowerShell process, elevated through UAC when `elevate` is set.
/// With `wait`, blocks until it finishes and returns its exit code.
fn launch_stub(stub: &str, elevate: bool, wait: bool) -> Result<i32, CommandError> {
    let verb = if elevate { "-Verb RunAs " } else { "" };
    let wait_flags = if wait { "-Wait -PassThru " } else { "" };
    let launcher = [
        "$ErrorActionPreference = 'Stop'".to_string(),
        format!(
            "try {{ $p = Start-Process -FilePath 'powershell.exe' {verb}{wait_flags}-WindowStyle Hidden \
             -ArgumentList '-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-WindowStyle','Hidden',\
             '-EncodedCommand','{}' }}",
            encode_ps(stub)
        ),
        format!(
            "catch {{ $e = $_.Exception; \
             while ($e -and -not ($e -is [ComponentModel.Win32Exception])) {{ $e = $e.InnerException }}; \
             if ($e -and $e.NativeErrorCode -eq 1223) {{ exit {EXIT_UAC_DECLINED} }}; \
             [Console]::Error.WriteLine($_.Exception.Message); exit {EXIT_START_FAILED} }}"
        ),
        (if wait { "exit $p.ExitCode" } else { "exit 0" }).to_string(),
    ]
    .join("\n");

    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-EncodedCommand", &encode_ps(&launcher)])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to start PowerShell: {e}"))?;

    match output.status.code().unwrap_or(-1) {
        EXIT_UAC_DECLINED => Err(CommandError::ElevationDeclined),
        EXIT_START_FAILED => Err(format!(
            "Failed to start elevated process: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into()),
        code => Ok(code),
    }
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

fn start_helper(exe: &Path, elevate: bool) -> Result<ElevatedHelper, CommandError> {
    check_exe_is_protected(exe)?;

    let cmd_dir = new_private_dir("mula_smartd")?;
    let token = crate::util::random_hex(24);
    let script = ps_script(
        HELPER_SCRIPT,
        &[
            ("Dir", &cmd_dir.to_string_lossy()),
            ("Exe", &exe.to_string_lossy()),
            ("Token", &token),
            ("ParentPid", &std::process::id().to_string()),
        ],
    );
    let stub = write_script_with_stub(&cmd_dir, &script)?;

    log::info!("Starting elevated smartctl helper in {}", cmd_dir.display());
    launch_stub(&stub, elevate, false)?;

    // Allows time for answering the UAC prompt.
    if !wait_for_file(&cmd_dir, &cmd_dir.join("ready"), Duration::from_secs(60)) {
        let _ = std::fs::remove_dir_all(&cmd_dir);
        return Err("Elevated helper did not become ready".into());
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
    // Written atomically: the helper must never pick up a half-written command.
    crate::util::write_atomic(&cmd_file, &json)?;

    if !wait_for_file(&helper.cmd_dir, &res_file, timeout) {
        let _ = std::fs::remove_file(&cmd_file);
        return Err("Elevated helper did not respond".to_string());
    }

    let bytes = std::fs::read(&res_file).map_err(|e| format!("Failed to read helper response: {e}"))?;
    let _ = std::fs::remove_file(&res_file);
    serde_json::from_slice(&bytes).map_err(|e| format!("Failed to parse helper response: {e}"))
}

/// Forgets a helper and removes its command directory.
fn discard(helper: Option<ElevatedHelper>) {
    if let Some(h) = helper {
        let _ = std::fs::remove_dir_all(&h.cmd_dir);
    }
}

pub fn stop_helper() {
    let helper = crate::util::lock(&ELEVATED_HELPER).take();
    if let Some(h) = &helper {
        let _ = send_command(h, &[], true, Duration::from_secs(5));
    }
    discard(helper);
}

/// Runs smartctl with administrator rights through the long-lived helper.
pub fn run_smartctl_elevated(exe: &Path, args: &[&str]) -> Result<String, CommandError> {
    let mut guard = crate::util::lock(&ELEVATED_HELPER);

    // A helper is bound to one executable; restart it if smartctl moved, or if it died.
    let reusable = guard.as_ref().is_some_and(|h| h.exe == exe && h.cmd_dir.join("ready").exists());
    if !reusable {
        discard(guard.take());
        *guard = Some(start_helper(exe, true).inspect_err(|e| log::error!("Failed to start elevated helper: {e}"))?);
    }

    let helper = guard.as_ref().unwrap();
    let response = match send_command(helper, args, false, Duration::from_secs(60)) {
        Ok(r) => r,
        Err(e) => {
            // Most likely the helper exited; drop it so the next call starts a fresh one.
            discard(guard.take());
            return Err(e.into());
        }
    };

    if !response.ok {
        return Err(format!("Elevated helper refused the command: {}", response.error).into());
    }
    if response.output.trim().is_empty() {
        return Err("command returned no output".into());
    }
    Ok(response.output)
}

/// Runs `template` once in its own elevated PowerShell (one UAC prompt), waiting for it to
/// finish. `vars` become PowerShell variables (see [`ps_script`]); `$ResultPath` names a file
/// the script should write a human-readable message to. It should exit 0 on success. Returns
/// that message.
pub fn run_elevated_script(template: &str, vars: &[(&str, &str)]) -> Result<String, CommandError> {
    run_script(template, vars, true)
}

fn run_script(template: &str, vars: &[(&str, &str)], elevate: bool) -> Result<String, CommandError> {
    let dir = new_private_dir("mula_elevated")?;
    let result_path = dir.join("result.txt");
    let result_path_text = result_path.to_string_lossy();
    let mut all_vars = vec![("ResultPath", result_path_text.as_ref())];
    all_vars.extend_from_slice(vars);
    let script = ps_script(template, &all_vars);

    let outcome = write_script_with_stub(&dir, &script)
        .map_err(CommandError::from)
        .and_then(|stub| launch_stub(&stub, elevate, true));
    let message = std::fs::read(&result_path)
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);

    match outcome? {
        0 => Ok(message),
        87 => Err("Elevated script failed its integrity check and was not run".into()),
        code if message.is_empty() => Err(format!("Elevated script failed (exit code {code})").into()),
        _ => Err(message.into()),
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

        for args in [&["--json=o", "-a", "-d", "sat", "/dev/sda"][..], &["--json=o", "-c", "-l", "selftest", "-d", "nvme", "/dev/pd0"]] {
            let res = send_command(&helper, args, false, Duration::from_secs(30)).unwrap();
            assert!(res.ok, "{args:?} should be allowed: {}", res.error);
        }

        let shapes: &[&[&str]] = &[
            &["/c", "calc.exe"],
            &["-a", "-d", "sat", r"C:\Windows\notepad.exe"],
            &["-a", "-d", "sat & calc", "/dev/sda"],
            &["-s", "on", "-d", "sat", "/dev/sda"],
            &["-d", "sat", "/dev/sda"],
            &["--json=o"],
            &["--json=o", "--json=o", "-a", "-d", "sat", "/dev/sda"],
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
    fn helper_answers_without_polling_delays() {
        let exe = where_exe();
        let helper = start_helper(&exe, false).expect("helper should start unelevated");
        // Warm up PowerShell's first command, then time a few round trips.
        send_command(&helper, &["--scan"], false, Duration::from_secs(30)).unwrap();
        let start = Instant::now();
        for _ in 0..3 {
            assert_eq!(send_command(&helper, &["-x"], false, Duration::from_secs(30)).unwrap().error, "command not allowed: smartctl -x");
        }
        let per_command = start.elapsed() / 3;
        // The old 250 ms + 100 ms polling averaged ~175 ms of pure waiting per command.
        assert!(per_command < Duration::from_millis(150), "round trip took {per_command:?}");
        send_command(&helper, &[], true, Duration::from_secs(30)).unwrap();
        let _ = std::fs::remove_dir_all(&helper.cmd_dir);
    }

    #[test]
    fn waiting_for_a_file_wakes_when_it_is_renamed_into_place_or_times_out() {
        let dir = crate::test_support::TempDir::new("wait_for_file");
        let target = dir.join("res_1.json");
        let writer = std::thread::spawn({
            let (dir, target) = (dir.path().to_path_buf(), target.clone());
            move || {
                std::thread::sleep(Duration::from_millis(200));
                std::fs::write(dir.join("other.tmp"), b"noise").unwrap();
                std::fs::write(dir.join("res_1.tmp"), b"{}").unwrap();
                std::fs::rename(dir.join("res_1.tmp"), &target).unwrap();
            }
        });
        let start = Instant::now();
        assert!(wait_for_file(dir.path(), &target, Duration::from_secs(10)));
        assert!(start.elapsed() < Duration::from_secs(2), "woke after {:?}", start.elapsed());
        writer.join().unwrap();

        let start = Instant::now();
        assert!(!wait_for_file(dir.path(), &dir.join("never.json"), Duration::from_millis(300)));
        assert!(start.elapsed() >= Duration::from_millis(300));

        // An existing file returns at once; a missing folder falls back to polling.
        assert!(wait_for_file(dir.path(), &target, Duration::ZERO));
        assert!(!wait_for_file(&dir.join("gone"), &dir.join("gone").join("x"), Duration::from_millis(200)));
    }

    #[test]
    fn helper_refuses_executables_in_user_folders() {
        let exe = std::env::temp_dir().join("smartctl.exe");
        assert!(start_helper(&exe, false).is_err());
        assert!(start_helper(Path::new("smartctl"), false).is_err());
    }

    #[test]
    fn one_shot_script_reports_message_and_exit_code() {
        let ok = run_script("[IO.File]::WriteAllText($ResultPath, 'it''s done'); exit 0", &[], false);
        assert_eq!(ok.unwrap(), "it's done");

        let failed = run_script("[IO.File]::WriteAllText($ResultPath, 'boom'); exit 1", &[], false);
        assert_eq!(failed.unwrap_err(), CommandError::from("boom"));
    }

    #[test]
    fn script_variables_are_passed_literally() {
        let value = "O'Brien $env:USERNAME; Remove-Item C:\\x";
        let echoed = run_script("[IO.File]::WriteAllText($ResultPath, $Value); exit 0", &[("Value", value)], false);
        assert_eq!(echoed.unwrap(), value);
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

#[cfg(test)]
mod encoding_tests {
    use super::*;

    #[test]
    fn scripts_get_their_variables_declared_first() {
        assert_eq!(
            ps_script("Write-Output $A", &[("A", r"C:\O'Brien"), ("B", "1")]),
            "$A = 'C:\\O''Brien'\n$B = '1'\nWrite-Output $A"
        );
    }

    #[test]
    fn powershell_literals_escape_single_quotes() {
        assert_eq!(ps_quote("plain"), "'plain'");
        assert_eq!(ps_quote("it's"), "'it''s'");
        assert_eq!(ps_quote("''"), "''''''");
    }

    #[test]
    fn encoded_commands_are_base64_utf16le() {
        assert_eq!(encode_ps("ab"), "YQBiAA==");
        let decoded = base64::engine::general_purpose::STANDARD.decode(encode_ps("Ž1")).unwrap();
        assert_eq!(decoded, [0x7D, 0x01, 0x31, 0x00]);
    }

    #[test]
    fn executables_in_protected_locations_are_accepted() {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        assert!(check_exe_is_protected(&Path::new(&windir).join("System32").join("where.exe")).is_ok());
        if let Some(home) = dirs::home_dir() {
            assert!(check_exe_is_protected(&home.join("scoop").join("smartctl.exe")).is_err());
        }
    }
}
