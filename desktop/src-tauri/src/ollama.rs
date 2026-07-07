//! Ollama lifecycle management for the Tauri desktop shell.
//!
//! Responsibilities (all fully automatic, no user action required):
//!
//! 1. **Detect** — check if the Ollama API is already reachable on port 11434.
//! 2. **Install** — if the binary is not present, download and silently install
//!    Ollama from the official source (curl install.sh on macOS/Linux, PowerShell
//!    + OllamaSetup.exe /S on Windows).
//! 3. **Start** — if the API is not reachable but the binary exists (or was just
//!    installed), spawn `ollama serve` as a child process.
//! 4. **Pull model** — in the background, pull the model configured in settings
//!    (default `llama3.2`) if it is not already present.
//! 5. **Stop** — when the app closes, if we started Ollama, kill it so it does
//!    not keep running in the background.  If Ollama was already running before
//!    the app launched (started externally or by the OS), we leave it running.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Default model to pull.  Matches `config/settings.yaml` → `llm.model`.
pub const DEFAULT_MODEL: &str = "llama3.2";
const OLLAMA_HOST: &str = "127.0.0.1:11434";

// ── Detection ──────────────────────────────────────────────────────────────

/// True if anything is already listening on the Ollama port.
pub fn is_api_reachable() -> bool {
    std::net::TcpStream::connect_timeout(
        &OLLAMA_HOST.parse().expect("constant address"),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// Wait up to `timeout_secs` for the Ollama API to become reachable.
pub fn wait_for_api(timeout_secs: u64, poll_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    while std::time::Instant::now() < deadline {
        if is_api_reachable() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(poll_ms));
    }
    false
}

/// Search PATH and well-known install paths for the `ollama` binary.
pub fn find_binary() -> Option<PathBuf> {
    // 1. PATH lookup.
    let locator = if cfg!(windows) { "where" } else { "which" };
    if let Ok(out) = Command::new(locator).arg("ollama").output() {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !path.is_empty() {
                let pb = PathBuf::from(&path);
                if pb.is_file() {
                    return Some(pb);
                }
            }
        }
    }

    // 2. Well-known install locations per OS.
    #[cfg(target_os = "macos")]
    {
        for p in [
            "/usr/local/bin/ollama",
            "/opt/homebrew/bin/ollama",
            "/Applications/Ollama.app/Contents/Resources/ollama",
        ] {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                return Some(pb);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let p = PathBuf::from(&local)
                .join("Programs")
                .join("Ollama")
                .join("ollama.exe");
            if p.is_file() {
                return Some(p);
            }
        }
        for p in [
            r"C:\Program Files\Ollama\ollama.exe",
            r"C:\Program Files (x86)\Ollama\ollama.exe",
        ] {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                return Some(pb);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        for p in ["/usr/local/bin/ollama", "/usr/bin/ollama"] {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                return Some(pb);
            }
        }
    }

    None
}

// ── Install ────────────────────────────────────────────────────────────────

/// Download and silently install Ollama.
/// `progress` is called with human-readable status messages during the install.
/// Returns the path to the installed binary on success, or an error string.
/// A failed install is non-fatal — the caller should continue without the LLM layer.
pub fn install(progress: impl Fn(&str)) -> Result<PathBuf, String> {
    _install_for_os(progress)
}

#[cfg(target_os = "macos")]
fn _install_for_os(progress: impl Fn(&str)) -> Result<PathBuf, String> {
    // Try Homebrew first (no sudo needed, user-level install).
    if let Ok(out) = Command::new("brew").args(["--version"]).output() {
        if out.status.success() {
            progress("Installing Ollama via Homebrew…");
            let status = Command::new("brew")
                .args(["install", "ollama"])
                .status()
                .map_err(|e| format!("brew install failed: {e}"))?;
            if status.success() {
                return find_binary().ok_or_else(|| "brew install succeeded but binary not found".into());
            }
            // Homebrew failed (e.g. already installed but missing from PATH) — fall through.
        }
    }
    // Fall back to the official install script.
    progress("Downloading Ollama installer (may request password)…");
    let tmp = std::env::temp_dir().join("ollama_install.sh");
    let dl = Command::new("curl")
        .args(["-fsSL", "https://ollama.com/install.sh", "-o"])
        .arg(&tmp)
        .output()
        .map_err(|e| format!("curl unavailable: {e}"))?;
    if !dl.status.success() {
        return Err(format!(
            "Ollama download failed: {}",
            String::from_utf8_lossy(&dl.stderr).trim()
        ));
    }
    progress("Installing Ollama…");
    let status = Command::new("sh")
        .arg(&tmp)
        .status()
        .map_err(|e| format!("install script failed: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    if !status.success() {
        return Err("Ollama install script exited with error".into());
    }
    find_binary().ok_or_else(|| "Ollama installed but binary not found in PATH".into())
}

#[cfg(target_os = "windows")]
const OLLAMA_WINDOWS_INSTALLER_URL: &str = "https://ollama.com/download/OllamaSetup.exe";

/// True if `path` looks like a Windows PE executable (not an HTML error page saved as .exe).
#[cfg(target_os = "windows")]
fn is_valid_windows_installer(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    // Real OllamaSetup.exe is tens of MB; HTML landing pages are tiny.
    if meta.len() < 5 * 1024 * 1024 {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    bytes.len() >= 2 && bytes[0] == b'M' && bytes[1] == b'Z'
}

#[cfg(target_os = "windows")]
fn _install_for_os(progress: impl Fn(&str)) -> Result<PathBuf, String> {
    // Ollama on Windows uses Inno Setup — NOT the HTML page at /download/windows.
    progress("Downloading Ollama installer…");
    let tmp = std::env::temp_dir().join("OllamaSetup.exe");
    let url = OLLAMA_WINDOWS_INSTALLER_URL;
    let ps_script = format!(
        "$ProgressPreference = 'SilentlyContinue'; \
         $uri = '{url}'; \
         $out = '{}'; \
         Invoke-WebRequest -Uri $uri -OutFile $out -UseBasicParsing;",
        tmp.display().to_string().replace('\'', "''")
    );
    let dl = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
        .status()
        .map_err(|e| format!("PowerShell unavailable: {e}"))?;
    if !dl.success() {
        return Err(format!(
            "Ollama download failed (expected 64-bit installer from {url})"
        ));
    }
    if !is_valid_windows_installer(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(
            "Downloaded file is not a valid Ollama installer (got HTML or a truncated file). \
             Check your network connection and try again."
                .into(),
        );
    }
    progress("Installing Ollama (silent)…");
    // Inno Setup silent flags (official install.ps1) — /S is for NSIS and does not work here.
    let status = Command::new(&tmp)
        .args(["/VERYSILENT", "/NORESTART", "/SUPPRESSMSGBOXES"])
        .status()
        .map_err(|e| format!("OllamaSetup.exe failed to start: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    if !status.success() {
        return Err(format!(
            "Ollama installer exited with status {}",
            status.code().unwrap_or(-1)
        ));
    }
    // Give the installer a moment to write the binary.
    std::thread::sleep(Duration::from_secs(3));
    find_binary().ok_or_else(|| "Ollama installed but binary not found".into())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn _install_for_os(progress: impl Fn(&str)) -> Result<PathBuf, String> {
    progress("Downloading & installing Ollama…");
    let status = Command::new("sh")
        .args(["-c", "curl -fsSL https://ollama.com/install.sh | sh"])
        .status()
        .map_err(|e| format!("sh unavailable: {e}"))?;
    if !status.success() {
        return Err("Ollama install failed".into());
    }
    find_binary().ok_or_else(|| "Ollama installed but binary not found".into())
}

// ── Lifecycle ──────────────────────────────────────────────────────────────

/// Ensure at most one `llama-server` process is running before we start Ollama.
/// Ollama spawns `llama-server` workers; orphaned duplicates from prior runs can
/// pile up and waste RAM. Keeps the oldest PID and terminates the rest.
pub fn dedupe_llama_servers(on_status: impl Fn(&str)) -> usize {
    let mut pids = list_llama_server_pids();
    if pids.len() <= 1 {
        return 0;
    }
    pids.sort_unstable();
    let keep = pids[0];
    let mut killed = 0usize;
    for pid in pids.into_iter().skip(1) {
        if kill_process(pid) {
            killed += 1;
        }
    }
    if killed > 0 {
        on_status(&format!(
            "Stopped {killed} duplicate llama-server process(es); kept PID {keep}"
        ));
        std::thread::sleep(Duration::from_millis(400));
    }
    killed
}

#[cfg(unix)]
fn list_llama_server_pids() -> Vec<u32> {
    let Ok(out) = Command::new("pgrep").args(["-x", "llama-server"]).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_pid_lines(&out.stdout)
}

#[cfg(windows)]
fn list_llama_server_pids() -> Vec<u32> {
    let Ok(out) = Command::new("tasklist")
        .args([
            "/FI",
            "IMAGENAME eq llama-server.exe",
            "/FO",
            "CSV",
            "/NH",
        ])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut pids = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("INFO:") {
            continue;
        }
        // "llama-server.exe","1234","Console",...
        let pid_str = line
            .split(',')
            .nth(1)
            .and_then(|s| s.trim_matches('"').parse::<u32>().ok());
        if let Some(pid) = pid_str {
            pids.push(pid);
        }
    }
    pids
}

fn parse_pid_lines(stdout: &[u8]) -> Vec<u32> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect()
}

#[cfg(unix)]
fn kill_process(pid: u32) -> bool {
    Command::new("kill")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn kill_process(pid: u32) -> bool {
    Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Spawn `ollama serve` in the background.
/// Returns the child process on success.  Stdout/stderr are discarded so the
/// process runs silently without keeping a console window open.
pub fn start_server(bin: &PathBuf) -> Option<Child> {
    Command::new(bin)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// Pull `model` in the background (fire-and-forget, non-blocking).
/// Calls `on_status` with progress messages.  A missing or stale model does not
/// block the bot — the LLM regime layer already falls back to neutral.
pub fn pull_model_background(
    bin: PathBuf,
    model: String,
    on_status: impl Fn(&str) + Send + 'static,
) {
    std::thread::spawn(move || {
        // Check whether the model is already present.
        let already_present = Command::new(&bin)
            .arg("list")
            .output()
            .map(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                out.contains(&model)
            })
            .unwrap_or(false);

        if already_present {
            on_status(&format!("Ollama model '{model}' already present"));
            return;
        }

        on_status(&format!(
            "Pulling Ollama model '{model}' (~2 GB, first time only) — trading continues in the background…"
        ));

        let result = Command::new(&bin)
            .args(["pull", &model])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        match result {
            Ok(s) if s.success() => on_status(&format!("Ollama model '{model}' ready")),
            Ok(_) => on_status(&format!("Ollama pull for '{model}' failed — LLM regime will stay neutral")),
            Err(e) => on_status(&format!("Ollama pull error: {e} — LLM regime will stay neutral")),
        }
    });
}

// ── OllamaHandle (Tauri managed state) ────────────────────────────────────

/// Tauri managed state.  Holds the child process we spawned (if any) and a
/// flag indicating whether we are responsible for its lifecycle.
pub struct OllamaHandle {
    child: Mutex<Option<Child>>,
    /// True only if we started Ollama ourselves — we must not kill it otherwise.
    we_started: AtomicBool,
}

impl OllamaHandle {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            we_started: AtomicBool::new(false),
        }
    }

    /// Record the child process we spawned.
    pub fn set_child(&self, child: Child) {
        if let Ok(mut lock) = self.child.lock() {
            *lock = Some(child);
        }
        self.we_started.store(true, Ordering::SeqCst);
    }

    /// Whether we own the running Ollama process.
    #[allow(dead_code)]
    pub fn we_started(&self) -> bool {
        self.we_started.load(Ordering::SeqCst)
    }

    /// Kill the Ollama process we started (no-op if we did not start it).
    /// Called when the app is closing.
    pub fn shutdown(&self) {
        if !self.we_started.load(Ordering::SeqCst) {
            return;
        }
        if let Ok(mut lock) = self.child.lock() {
            if let Some(mut child) = lock.take() {
                let _ = child.kill();
                // Wait briefly so the OS reclaims resources cleanly.
                let _ = child.wait();
                eprintln!("[ollama] Stopped Ollama (we started it)");
            }
        }
    }
}

impl Default for OllamaHandle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod dedupe_tests {
    use super::parse_pid_lines;

    #[test]
    fn parse_pid_lines_reads_pgrep_output() {
        assert_eq!(parse_pid_lines(b"42\n1001\n"), vec![42, 1001]);
        assert_eq!(parse_pid_lines(b""), Vec::<u32>::new());
        assert_eq!(parse_pid_lines(b"  99 \n"), vec![99]);
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn rejects_html_saved_as_exe() {
        let dir = std::env::temp_dir().join("ollama_install_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("fake.exe");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"<!DOCTYPE html><html>").unwrap();
        assert!(!is_valid_windows_installer(&path));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn accepts_mz_header_and_min_size() {
        let dir = std::env::temp_dir().join("ollama_install_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("fake_pe.exe");
        let mut f = std::fs::File::create(&path).unwrap();
        let mut buf = vec![b'M', b'Z'];
        buf.resize(5 * 1024 * 1024 + 1, 0);
        f.write_all(&buf).unwrap();
        assert!(is_valid_windows_installer(&path));
        let _ = std::fs::remove_file(&path);
    }
}
