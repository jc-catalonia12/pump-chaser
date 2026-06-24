//! Resolve config, web assets, and user data paths for dev, CLI, and packaged installers.

use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_REL: &str = "config/settings.yaml";
const APP_DIR_NAME: &str = "MEXC Trading Bot";
/// Bundled ML models copied into the installer via `release-assets/models/`.
const BUNDLED_MODELS_REL: &str = "release-assets/models";

/// Tauri bundles `../../config` as `Resources/_up_/_up_/config`. Search common layouts.
fn find_resource_root_in_dir(base: &Path) -> Option<PathBuf> {
    if base.join(CONFIG_REL).is_file() {
        return Some(base.to_path_buf());
    }
    for rel in ["_up_/_up_", "_up_"] {
        let nested = base.join(rel);
        if nested.join(CONFIG_REL).is_file() {
            return Some(nested);
        }
    }
    let Ok(entries) = fs::read_dir(base) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.join(CONFIG_REL).is_file() {
                return Some(path);
            }
            if let Ok(sub) = fs::read_dir(&path) {
                for sub_entry in sub.flatten() {
                    let sub_path = sub_entry.path();
                    if sub_path.is_dir() && sub_path.join(CONFIG_REL).is_file() {
                        return Some(sub_path);
                    }
                }
            }
        }
    }
    None
}

/// Find the dev project root (directory containing `config/settings.yaml`).
pub fn discover_project_root() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("MEXC_BOT_HOME") {
        let p = PathBuf::from(&home);
        if p.join(CONFIG_REL).is_file() {
            return Some(p);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(mut dir) = exe.parent().map(Path::to_path_buf) {
            for _ in 0..8 {
                candidates.push(dir.clone());
                if !dir.pop() {
                    break;
                }
            }
        }
    }

    for dir in candidates {
        if dir.join(CONFIG_REL).is_file() {
            return Some(dir);
        }
    }
    None
}

/// User-writable data directory (secrets, SQLite, ML models). Never bundled in the installer.
pub fn app_data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("MEXC_BOT_DATA_DIR") {
        return PathBuf::from(d);
    }
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR_NAME)
}

/// True when running from a Tauri `.app` / `resources/` install (not `cargo run` from source).
pub fn is_packaged_install() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let path = exe.to_string_lossy();
    if path.contains(".app/Contents/MacOS/") {
        return true;
    }
    if let Some(parent) = exe.parent() {
        return find_resource_root_in_dir(&parent.join("resources")).is_some();
    }
    false
}

/// Bundled read-only resources (`config/`, `web/`) inside the installer.
pub fn discover_resource_root() -> Option<PathBuf> {
    if let Ok(r) = std::env::var("MEXC_BOT_RESOURCE_DIR") {
        let p = PathBuf::from(r);
        if p.join(CONFIG_REL).is_file() {
            return Some(p);
        }
    }

    let exe = std::env::current_exe().ok()?;

    // macOS: App.app/Contents/MacOS/exe → Contents/Resources/
    if let Some(parent) = exe.parent() {
        let macos = parent.parent()?.join("Resources");
        if let Some(root) = find_resource_root_in_dir(&macos) {
            return Some(root);
        }
    }

    // Windows / Linux: resources/ next to the executable
    if let Some(parent) = exe.parent() {
        let win = parent.join("resources");
        if let Some(root) = find_resource_root_in_dir(&win) {
            return Some(root);
        }
    }

    None
}

/// Prepare paths before config/secrets load. Safe to call in dev and packaged builds.
pub fn init_runtime_paths() {
    if std::env::var("MEXC_BOT_CONFIG").is_ok() {
        return;
    }

    if is_packaged_install() {
        setup_packaged_paths();
        return;
    }

    // Dev / `cargo run` from source tree
    if let Some(root) = discover_project_root() {
        if std::env::set_current_dir(&root).is_ok() {
            tracing::debug!("Working directory set to {}", root.display());
        }
    }
}

fn setup_packaged_paths() {
    let data_dir = app_data_dir();
    let _ = fs::create_dir_all(&data_dir);
    let _ = fs::create_dir_all(data_dir.join("config"));
    let _ = fs::create_dir_all(data_dir.join("models"));

    std::env::set_var("MEXC_BOT_DATA_DIR", data_dir.display().to_string());
    std::env::set_var(
        "MEXC_BOT_SECRETS_PATH",
        data_dir.join("secrets.json").display().to_string(),
    );

    let user_config = data_dir.join(CONFIG_REL);
    if !user_config.is_file() {
        if let Some(resource) = discover_resource_root() {
            std::env::set_var("MEXC_BOT_RESOURCE_DIR", resource.display().to_string());
            let bundled = resource.join(CONFIG_REL);
            if bundled.is_file() {
                let _ = fs::copy(&bundled, &user_config);
            }
        }
    }

    std::env::set_var("MEXC_BOT_CONFIG", user_config.display().to_string());

    if std::env::var("MEXC_BOT_RESOURCE_DIR").is_err() {
        if let Some(resource) = discover_resource_root() {
            std::env::set_var("MEXC_BOT_RESOURCE_DIR", resource.display().to_string());
        }
    }

    if let Some(resource) = discover_resource_root() {
        seed_bundled_models(&resource, &data_dir);
    }
}

/// Copy bundled ONNX / online model files into the user data folder on first launch.
/// Skips files that already exist so user-trained models are never overwritten.
fn seed_bundled_models(resource_root: &Path, data_dir: &Path) {
    let user_models = data_dir.join("models");
    let _ = fs::create_dir_all(&user_models);

    let bundled = resource_root.join(BUNDLED_MODELS_REL);
    if !bundled.is_dir() {
        return;
    }

    let Ok(entries) = fs::read_dir(&bundled) else {
        return;
    };
    for entry in entries.flatten() {
        let src = entry.path();
        if !src.is_file() {
            continue;
        }
        let Some(name) = src.file_name() else {
            continue;
        };
        let dst = user_models.join(name);
        if dst.is_file() {
            continue;
        }
        if fs::copy(&src, &dst).is_ok() {
            tracing::info!(
                "Seeded bundled model {} → {}",
                src.display(),
                dst.display()
            );
        }
    }
}

/// Legacy alias — calls [`init_runtime_paths`].
pub fn init_working_directory() {
    init_runtime_paths();
}

/// Directory served as the dashboard static files.
pub fn web_assets_dir() -> PathBuf {
    if let Ok(r) = std::env::var("MEXC_BOT_RESOURCE_DIR") {
        let web = PathBuf::from(r).join("web");
        if web.is_dir() {
            return web;
        }
    }
    if let Some(root) = discover_project_root() {
        let web = root.join("web");
        if web.is_dir() {
            return web;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web")
}

/// App icon for `/icon.png` (packaged resource or dev fallback).
pub fn app_icon_path() -> PathBuf {
    if let Ok(r) = std::env::var("MEXC_BOT_RESOURCE_DIR") {
        let root = PathBuf::from(&r);
        for rel in ["web/icon.png", "desktop/icon.png"] {
            let icon = root.join(rel);
            if icon.is_file() {
                return icon;
            }
        }
    }
    if let Some(root) = discover_project_root() {
        let icon = root.join("desktop/icon.png");
        if icon.is_file() {
            return icon;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("desktop/icon.png")
}

/// Resolve a relative storage/model path against the user data directory when packaged.
pub fn resolve_data_path(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        return p;
    }
    if is_packaged_install() || std::env::var("MEXC_BOT_DATA_DIR").is_ok() {
        return app_data_dir().join(&p);
    }
    if let Some(root) = discover_project_root() {
        return root.join(&p);
    }
    p
}

/// Rewrite relative paths in loaded config for packaged installs.
pub fn normalize_config_paths(cfg: &mut crate::config::AppConfig) {
    if !is_packaged_install() && std::env::var("MEXC_BOT_DATA_DIR").is_err() {
        return;
    }
    cfg.storage.sqlite_path = resolve_data_path(&cfg.storage.sqlite_path)
        .to_string_lossy()
        .into_owned();
    if let Some(ref onnx) = cfg.ml.onnx_model_path {
        if !Path::new(onnx).is_absolute() {
            cfg.ml.onnx_model_path = Some(resolve_data_path(onnx).to_string_lossy().into_owned());
        }
    }
}
