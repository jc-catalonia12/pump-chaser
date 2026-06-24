//! Tauri desktop — opens the web UI, reusing an existing API server when present.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

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

fn spawn_api_server_if_needed() {
    // Resolve bundled resource + user data paths before the API thread starts.
    mexc_trading_bot::utils::init_runtime_paths();

    if mexc_trading_bot::server::is_api_reachable() {
        eprintln!(
            "API already running at {} — desktop will use it (stop duplicate `cargo run` if unintended)",
            mexc_trading_bot::server::default_api_url()
        );
        return;
    }

    std::thread::spawn(|| {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        mexc_trading_bot::server::init_tracing();
        if let Err(exc) = rt.block_on(mexc_trading_bot::server::run()) {
            if exc.to_string().contains("Address already in use") {
                eprintln!(
                    "Port 8001 in use — using existing API at {}",
                    mexc_trading_bot::server::default_api_url()
                );
            } else {
                eprintln!("API server exited: {exc}");
            }
        }
    });
}

fn main() {
    spawn_api_server_if_needed();

    if !mexc_trading_bot::server::wait_for_api(120, 250) {
        eprintln!(
            "Warning: API not reachable at {} — start with `cargo run` or restart the desktop app",
            mexc_trading_bot::server::default_api_url()
        );
    }

    tauri::Builder::default()
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.eval(WEBVIEW_GUARD);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
