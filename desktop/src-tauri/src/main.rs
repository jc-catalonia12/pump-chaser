//! Tauri desktop — splash screen while the embedded API server starts, then opens the dashboard.
//!
//! Startup sequence (5 steps):
//!   1. Prepare app data directory.
//!   2. Copy first-launch assets.
//!   3. Manage Ollama (install if absent → start → pull model in background).
//!   4. Start the local Axum API server on 127.0.0.1:8001.
//!   5. Open the dashboard webview; close the splash window.
//!
//! On exit, if we started Ollama ourselves, it is killed so it does not keep
//! running in the background.
//!
//! Auto-update checks run 6 s after the dashboard opens and show a dismissible
//! in-app banner when a new version is available on GitHub Releases.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ollama;

use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;
use url::Url;

const WEBVIEW_GUARD: &str = r#"
(function () {
  document.addEventListener('contextmenu', function (e) { e.preventDefault(); }, { capture: true });
  document.addEventListener('keydown', function (e) {
    var key = e.key || '';
    if (key === 'F12') { e.preventDefault(); return; }
    if (e.metaKey && e.altKey && /^(i|j|c)$/i.test(key)) { e.preventDefault(); return; }
    if (e.ctrlKey && e.shiftKey && /^(i|j|c)$/i.test(key)) { e.preventDefault(); }
  }, { capture: true });
})();
"#;

// ── Logging / splash helpers ───────────────────────────────────────────────

fn log_startup(message: &str) {
    eprintln!("{message}");
    mexc_trading_bot::utils::append_startup_log(message);
}

fn splash_status(app: &tauri::AppHandle, message: &str, step: u8) {
    let msg = message.replace('\\', "\\\\").replace('\'', "\\'");
    if let Some(splash) = app.get_webview_window("splash") {
        let _ = splash.eval(format!("window.setStartupStatus('{msg}', {step});"));
    }
}

fn splash_error(app: &tauri::AppHandle, message: &str) {
    let msg = message
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n");
    if let Some(splash) = app.get_webview_window("splash") {
        let _ = splash.eval(format!("window.setStartupError('{msg}');"));
    }
}

// ── API server ─────────────────────────────────────────────────────────────

fn spawn_api_server_if_needed() {
    if mexc_trading_bot::server::is_same_version_api_reachable() {
        log_startup(&format!(
            "API already running at {} (v{}) — reusing existing server",
            mexc_trading_bot::server::default_api_url(),
            mexc_trading_bot::server::package_version()
        ));
        return;
    }

    if mexc_trading_bot::server::is_api_reachable() {
        let remote = mexc_trading_bot::server::remote_api_version().unwrap_or_else(|| "?".into());
        log_startup(&format!(
            "Stale API on {} reports v{remote}, this build is v{} — not reusing (will try to bind)",
            mexc_trading_bot::server::default_api_url(),
            mexc_trading_bot::server::package_version()
        ));
    }

    log_startup("Spawning embedded API server thread");
    std::thread::spawn(|| {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        mexc_trading_bot::server::init_tracing();
        if let Err(exc) = rt.block_on(mexc_trading_bot::server::run()) {
            let detail = exc.to_string();
            if detail.contains("Address already in use") {
                let remote = mexc_trading_bot::server::remote_api_version()
                    .unwrap_or_else(|| "unknown".into());
                log_startup(&format!(
                    "Port 8001 still held by another process (remote health v{remote}). \
                     Close other MEXC Trading Bot / cargo run instances and relaunch."
                ));
            } else {
                log_startup(&format!("API server exited: {detail}"));
            }
        }
    });
}

// ── Dashboard ──────────────────────────────────────────────────────────────

fn open_dashboard(app: &tauri::AppHandle) {
    // Cache-bust the document URL so WebView2 cannot keep a stale index.html
    // from a previous install (that HTML is what contains the Virtual Assistant FAB).
    let api_url = format!(
        "{}/?v={}",
        mexc_trading_bot::server::default_api_url().trim_end_matches('/'),
        mexc_trading_bot::server::package_version()
    );
    let Ok(parsed) = Url::parse(&api_url) else {
        splash_error(app, "Invalid API URL configured for the dashboard.");
        return;
    };

    let Some(main) = app.get_webview_window("main") else {
        splash_error(app, "Main window failed to initialize.");
        return;
    };

    // Drop prior WebView2 HTTP cache for this app so upgrades load bundled UI.
    let _ = main.clear_all_browsing_data();

    if let Err(e) = main.navigate(parsed) {
        splash_error(
            app,
            &format!("Could not open dashboard at {api_url}:\n{e}"),
        );
        return;
    }

    let _ = main.eval(WEBVIEW_GUARD);
    let _ = main.show();
    let _ = main.set_focus();

    if let Some(splash) = app.get_webview_window("splash") {
        let _ = splash.close();
    }

    log_startup("Dashboard opened");

    // Kick off background update check 6 s after the dashboard is visible.
    let check_app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(6));
        run_update_check(check_app);
    });
}

// ── Auto-updater ───────────────────────────────────────────────────────────

/// True when `tauri.conf.json` has real updater credentials (not the dev placeholders).
/// Local `cargo run` builds keep `TAURI_UPDATER_PUBKEY_PLACEHOLDER` and a GitHub URL
/// with `/OWNER/` — those 404 and spam the log if we call `updater.check()`.
fn updater_is_configured(app: &tauri::AppHandle) -> bool {
    let Some(updater) = app.config().plugins.0.get("updater") else {
        return false;
    };
    let pubkey = updater
        .get("pubkey")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if pubkey.is_empty() || pubkey.contains("PLACEHOLDER") {
        return false;
    }
    let Some(endpoints) = updater.get("endpoints").and_then(|v| v.as_array()) else {
        return false;
    };
    if endpoints.is_empty() {
        return false;
    }
    endpoints.iter().all(|ep| {
        let url = ep.as_str().unwrap_or("");
        !url.is_empty()
            && !url.contains("/OWNER/")
            && !url.contains("YOUR_USERNAME")
            && !url.contains("YOUR_GITHUB_USERNAME")
    })
}

/// Check for a newer version on GitHub Releases and show the in-app banner.
/// All errors are logged and ignored so a missing/misconfigured updater never
/// interrupts trading.
fn run_update_check(app: tauri::AppHandle) {
    if !updater_is_configured(&app) {
        log_startup("Updater skipped — configure plugins.updater in tauri.conf.json (see README)");
        return;
    }

    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            log_startup(&format!("Update-check runtime failed to start: {e}"));
            return;
        }
    };

    rt.block_on(async move {
        use tauri_plugin_updater::UpdaterExt;

        let updater = match app.updater() {
            Ok(u) => u,
            Err(e) => {
                log_startup(&format!("Updater skipped: {e}"));
                return;
            }
        };

        match updater.check().await {
            Ok(Some(update)) => {
                let version = update.version.clone();
                let notes = update.body.clone().unwrap_or_default();
                log_startup(&format!("Update available: v{version}"));

                if let Some(win) = app.get_webview_window("main") {
                    let ver_esc = version.replace('\\', "\\\\").replace('\'', "\\'");
                    let notes_esc = notes
                        .replace('\\', "\\\\")
                        .replace('\'', "\\'")
                        .replace('\n', " ");
                    let _ = win.eval(&format!(
                        "window.showUpdateBanner('{ver_esc}', '{notes_esc}');"
                    ));
                }
            }
            Ok(None) => log_startup("App is up to date"),
            Err(e) => log_startup(&format!("Update check failed (non-fatal): {e}")),
        }
    });
}

/// Tauri command: download the pending update and install it.
#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;

    if !updater_is_configured(&app) {
        return Err(
            "Auto-update is not configured. Set plugins.updater in tauri.conf.json (see README)."
                .into(),
        );
    }

    let updater = app
        .updater()
        .map_err(|e| format!("Updater unavailable: {e}"))?;

    let update = updater
        .check()
        .await
        .map_err(|e| format!("Update check failed: {e}"))?;

    let Some(update) = update else {
        return Err("No update available".into());
    };

    update
        .download_and_install(
            |chunk_len, total| {
                let _ = (chunk_len, total);
            },
            || {},
        )
        .await
        .map_err(|e| format!("Install failed: {e}"))?;

    app.restart();
}

// ── Ollama step (step 3 of 5) ──────────────────────────────────────────────

/// Ensure the Ollama LLM service is running and, if needed, install it first.
/// This is intentionally resilient — any failure is logged and the bot
/// continues without the regime-filter layer.
fn setup_ollama(app: &tauri::AppHandle, handle: Arc<ollama::OllamaHandle>) {
    ollama::dedupe_llama_servers(|msg| log_startup(&format!("[ollama] {msg}")));

    // If the API is already reachable (externally started service, OS autostart,
    // etc.) we do not manage its lifecycle at all.
    if ollama::is_api_reachable() {
        log_startup("Ollama already running — reusing external instance");
        // Pull model in background so it is ready for the first prediction.
        if let Some(bin) = ollama::find_binary() {
            ollama::pull_model_background(
                bin,
                ollama::DEFAULT_MODEL.to_string(),
                |msg| log_startup(&format!("[ollama] {msg}")),
            );
        }
        return;
    }

    // Try to find (or install) the binary.
    let bin_opt = match ollama::find_binary() {
        Some(bin) => {
            log_startup(&format!("Ollama binary found: {}", bin.display()));
            Some(bin)
        }
        None => {
            log_startup("Ollama not found — attempting automatic install");
            splash_status(app, "Installing Ollama (LLM regime layer)…", 3);

            match ollama::install(|msg| {
                log_startup(&format!("[ollama] {msg}"));
                splash_status(app, msg, 3);
            }) {
                Ok(bin) => {
                    log_startup(&format!(
                        "Ollama installed successfully: {}",
                        bin.display()
                    ));
                    Some(bin)
                }
                Err(e) => {
                    log_startup(&format!(
                        "Ollama install failed (non-fatal, LLM layer disabled): {e}"
                    ));
                    None
                }
            }
        }
    };

    let Some(bin) = bin_opt else {
        return;
    };

    // Start `ollama serve` and track the child so we can stop it on exit.
    splash_status(app, "Starting Ollama (LLM regime layer)…", 3);
    match ollama::start_server(&bin) {
        Some(child) => {
            handle.set_child(child);
            log_startup("Ollama server started by bot — will be stopped on exit");

            // Give it a moment to bind its port before proceeding.
            if !ollama::wait_for_api(15, 300) {
                log_startup("Ollama started but API not yet reachable — continuing anyway");
            }
        }
        None => {
            log_startup("Failed to spawn Ollama process — LLM regime layer disabled");
            return;
        }
    }

    // Pull the model in the background (non-blocking, may take several minutes).
    let app_clone = app.clone();
    ollama::pull_model_background(
        bin,
        ollama::DEFAULT_MODEL.to_string(),
        move |msg| {
            log_startup(&format!("[ollama] {msg}"));
            // Surface long-running model pull status in the status bar (best-effort).
            if let Some(win) = app_clone.get_webview_window("main") {
                let safe = msg.replace('\'', "\\'");
                let _ = win.eval(&format!(
                    "if(window.showOllamaStatus)window.showOllamaStatus('{safe}');"
                ));
            }
        },
    );
}

// ── Main startup sequence ──────────────────────────────────────────────────

fn run_startup_sequence(app: tauri::AppHandle, ollama_handle: Arc<ollama::OllamaHandle>) {
    std::thread::spawn(move || {
        splash_status(&app, "Preparing application data…", 1);
        log_startup("Desktop startup sequence began");

        splash_status(&app, "Copying settings and models (first launch)…", 2);
        std::thread::sleep(Duration::from_millis(150));

        // ── Step 3: Ollama ─────────────────────────────────────────────────
        splash_status(&app, "Checking Ollama (LLM regime layer)…", 3);
        setup_ollama(&app, ollama_handle);

        // ── Step 4: API server ─────────────────────────────────────────────
        splash_status(&app, "Starting local server on 127.0.0.1:8001…", 4);
        spawn_api_server_if_needed();

        let ready = mexc_trading_bot::server::wait_for_api(240, 250);
        if !ready {
            let log_hint = mexc_trading_bot::utils::app_data_dir()
                .join("startup.log")
                .display()
                .to_string();
            log_startup("API server did not become reachable within 60 seconds");
            let err_handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                splash_error(
                    &err_handle,
                    &format!(
                        "The local API server did not start.\n\n\
                         • Close any other copy of this app or `cargo run`\n\
                         • Restart the app\n\
                         • Check log: {log_hint}"
                    ),
                );
            });
            return;
        }

        if !mexc_trading_bot::server::is_same_version_api_reachable() {
            let remote = mexc_trading_bot::server::remote_api_version().unwrap_or_else(|| "?".into());
            let ours = mexc_trading_bot::server::package_version();
            let log_hint = mexc_trading_bot::utils::app_data_dir()
                .join("startup.log")
                .display()
                .to_string();
            log_startup(&format!(
                "API on :8001 is v{remote}, expected v{ours} — refusing to open stale UI"
            ));
            let err_handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                splash_error(
                    &err_handle,
                    &format!(
                        "Port 8001 is still serving an older bot (v{remote}).\n\
                         This install is v{ours}.\n\n\
                         1. Open Task Manager and end every \"MEXC Trading Bot\" process\n\
                         2. Also end any leftover `mexc-trading-bot` / cargo run\n\
                         3. Relaunch this app\n\n\
                         Log: {log_hint}"
                    ),
                );
            });
            return;
        }

        // ── Step 5: Open dashboard ─────────────────────────────────────────
        splash_status(&app, "Loading dashboard…", 5);
        log_startup(&format!(
            "API ready at {} (v{})",
            mexc_trading_bot::server::default_api_url(),
            mexc_trading_bot::server::package_version()
        ));

        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            open_dashboard(&handle);
        });
    });
}

// ── Entry point ────────────────────────────────────────────────────────────

fn configure_packaged_paths(app: &tauri::AppHandle) {
    if let Ok(resource_dir) = app.path().resource_dir() {
        log_startup(&format!("Tauri resource dir: {}", resource_dir.display()));
        mexc_trading_bot::utils::prime_packaged_install(&resource_dir);
    } else {
        log_startup("Tauri resource dir unavailable — using exe-path discovery");
    }
    mexc_trading_bot::utils::init_runtime_paths();
    if let Ok(config_path) = std::env::var("MEXC_BOT_CONFIG") {
        log_startup(&format!("Config path: {config_path}"));
    }
    if let Ok(resource_path) = std::env::var("MEXC_BOT_RESOURCE_DIR") {
        log_startup(&format!("Bundled resources: {resource_path}"));
    }
}

fn main() {
    log_startup("MEXC Trading Bot desktop process started");

    // Shared Ollama process handle — registered as Tauri managed state so the
    // exit handler can reach it even after setup() returns.
    let ollama_handle = Arc::new(ollama::OllamaHandle::new());
    let handle_for_setup = Arc::clone(&ollama_handle);

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(ollama_handle)
        .invoke_handler(tauri::generate_handler![install_update])
        .setup(move |app| {
            configure_packaged_paths(app.handle());
            splash_status(app.handle(), "Starting…", 1);
            run_startup_sequence(app.handle().clone(), Arc::clone(&handle_for_setup));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building tauri application");

    // Run the event loop. On exit, shut down the Ollama process we started.
    app.run(|app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(ollama) = app_handle.try_state::<Arc<ollama::OllamaHandle>>() {
                log_startup("App closing — stopping Ollama if we started it…");
                ollama.shutdown();
            }
        }
    });
}
