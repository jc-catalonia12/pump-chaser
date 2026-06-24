//! Tauri desktop — splash screen while the embedded API server starts, then opens the dashboard.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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

fn log_startup(message: &str) {
    eprintln!("{message}");
    mexc_trading_bot::utils::append_startup_log(message);
}

fn splash_status(app: &tauri::AppHandle, message: &str, step: u8) {
    let msg = message.replace('\\', "\\\\").replace('\'', "\\'");
    if let Some(splash) = app.get_webview_window("splash") {
        let _ = splash.eval(format!(
            "window.setStartupStatus('{msg}', {step});"
        ));
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

fn spawn_api_server_if_needed() {
    if mexc_trading_bot::server::is_api_reachable() {
        log_startup(&format!(
            "API already running at {} — reusing existing server",
            mexc_trading_bot::server::default_api_url()
        ));
        return;
    }

    log_startup("Spawning embedded API server thread");
    std::thread::spawn(|| {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        mexc_trading_bot::server::init_tracing();
        if let Err(exc) = rt.block_on(mexc_trading_bot::server::run()) {
            let detail = exc.to_string();
            if detail.contains("Address already in use") {
                log_startup(&format!(
                    "Port 8001 in use — using existing API at {}",
                    mexc_trading_bot::server::default_api_url()
                ));
            } else {
                log_startup(&format!("API server exited: {detail}"));
            }
        }
    });
}

fn open_dashboard(app: &tauri::AppHandle) {
    let api_url = mexc_trading_bot::server::default_api_url();
    let Ok(parsed) = Url::parse(&api_url) else {
        splash_error(app, "Invalid API URL configured for the dashboard.");
        return;
    };

    let Some(main) = app.get_webview_window("main") else {
        splash_error(app, "Main window failed to initialize.");
        return;
    };

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
}

fn run_startup_sequence(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        splash_status(&app, "Preparing application data…", 1);
        log_startup("Desktop startup sequence began");

        splash_status(&app, "Copying settings and models (first launch)…", 2);
        std::thread::sleep(Duration::from_millis(150));

        splash_status(&app, "Starting local server on 127.0.0.1:8001…", 3);
        spawn_api_server_if_needed();

        let ready = mexc_trading_bot::server::wait_for_api(240, 250);
        if !ready {
            let log_hint = mexc_trading_bot::utils::app_data_dir()
                .join("startup.log")
                .display()
                .to_string();
            log_startup("API server did not become reachable within 60 seconds");
            let handle = app.clone();
            let err_handle = handle.clone();
            let _ = handle.run_on_main_thread(move || {
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

        splash_status(&app, "Loading dashboard…", 4);
        log_startup(&format!(
            "API ready at {}",
            mexc_trading_bot::server::default_api_url()
        ));

        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            open_dashboard(&handle);
        });
    });
}

fn main() {
    mexc_trading_bot::utils::init_runtime_paths();
    log_startup("MEXC Trading Bot desktop process started");

    tauri::Builder::default()
        .setup(|app| {
            splash_status(app.handle(), "Starting…", 1);
            run_startup_sequence(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
