//! Resolve config, web assets, and user data paths for dev, CLI, and packaged installers.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CONFIG_REL: &str = "config/settings.yaml";
const APP_DIR_NAME: &str = "MEXC Trading Bot";
/// Bundled ML models copied into the installer via `release-assets/models/`.
const BUNDLED_MODELS_REL: &str = "release-assets/models";
/// Bundled seed database (signals, training history) via `release-assets/data/`.
const BUNDLED_DATA_REL: &str = "release-assets/data";
const BUNDLED_MANIFEST_REL: &str = "release-assets/seed.manifest";
const APPLIED_MANIFEST_NAME: &str = ".bundled_seed.json";
const SEED_DB_NAME: &str = "mexc_trading_bot.db";
/// Matches `config/settings.yaml` → `data/models/` (same as [`resolve_data_path`]).
const USER_MODELS_REL: &str = "data/models";

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

/// Candidate folders that may contain bundled `config/settings.yaml`.
fn packaged_resource_search_roots(exe: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let Some(parent) = exe.parent() else {
        return roots;
    };
    if parent.ends_with("MacOS") {
        if let Some(contents) = parent.parent() {
            roots.push(contents.join("Resources"));
        }
    }
    // Tauri v2 Windows / Linux: `resources/` next to the executable.
    roots.push(parent.join("resources"));
    // Tauri v2 also mirrors `../../` bundle paths as `_up_/_up_/` beside the exe.
    roots.push(parent.to_path_buf());
    // NSIS installs sometimes nest the exe one level deeper.
    if let Some(grandparent) = parent.parent() {
        roots.push(grandparent.join("resources"));
        roots.push(grandparent.to_path_buf());
    }
    roots
}

/// Walk *base* (depth-first, up to *max_depth*) looking for `config/settings.yaml`.
fn find_resource_root_recursive(base: &Path, max_depth: u32) -> Option<PathBuf> {
    if base.join(CONFIG_REL).is_file() {
        return Some(base.to_path_buf());
    }
    if max_depth == 0 {
        return None;
    }
    let Ok(entries) = fs::read_dir(base) else {
        return None;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_resource_root_recursive(&path, max_depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

/// Running `cargo run` / `cargo tauri dev` from the repo (not an installed bundle).
fn is_dev_source_tree() -> bool {
    discover_project_root()
        .is_some_and(|root| root.join("desktop/src-tauri/Cargo.toml").is_file())
}

/// Called from the Tauri desktop shell before [`init_runtime_paths`].
/// Resolves the installer bundle root via Tauri's `resource_dir()` so Windows
/// NSIS layouts are found reliably (exe-adjacent `_up_/_up_/` or `resources/`).
pub fn prime_packaged_install(resource_dir: &Path) {
    std::env::set_var("MEXC_BOT_PACKAGED", "1");
    if std::env::var("MEXC_BOT_RESOURCE_DIR").is_ok() {
        return;
    }
    if let Some(root) = find_resource_root_in_dir(resource_dir) {
        std::env::set_var("MEXC_BOT_RESOURCE_DIR", root.display().to_string());
        return;
    }
    if let Some(root) = find_resource_root_recursive(resource_dir, 6) {
        std::env::set_var("MEXC_BOT_RESOURCE_DIR", root.display().to_string());
    }
}

/// True when running from a Tauri installer (not `cargo run` from source).
pub fn is_packaged_install() -> bool {
    if std::env::var("MEXC_BOT_PACKAGED").is_ok() {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    if exe.to_string_lossy().contains(".app/Contents/MacOS/") {
        return true;
    }
    if is_dev_source_tree() {
        return false;
    }
    for base in packaged_resource_search_roots(&exe) {
        if find_resource_root_in_dir(&base).is_some() {
            return true;
        }
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
    for base in packaged_resource_search_roots(&exe) {
        if let Some(root) = find_resource_root_in_dir(&base) {
            return Some(root);
        }
    }

    None
}

/// Append a line to `%LOCALAPPDATA%/MEXC Trading Bot/startup.log` (desktop diagnostics).
pub fn append_startup_log(message: &str) {
    let log_path = app_data_dir().join("startup.log");
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let ts = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    let line = format!("[{ts}] {message}\n");
    use std::io::Write;
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = file.write_all(line.as_bytes());
    }
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
    let _ = fs::create_dir_all(data_dir.join("data"));
    let _ = fs::create_dir_all(user_models_dir(&data_dir));

    std::env::set_var("MEXC_BOT_DATA_DIR", data_dir.display().to_string());
    std::env::set_var(
        "MEXC_BOT_SECRETS_PATH",
        data_dir.join("secrets.json").display().to_string(),
    );

    if let Some(resource) = discover_resource_root() {
        std::env::set_var("MEXC_BOT_RESOURCE_DIR", resource.display().to_string());
        apply_bundled_seed(&resource, &data_dir);
        migrate_legacy_models(&data_dir);
    }

    let user_config = data_dir.join(CONFIG_REL);
    if !user_config.is_file() {
        if let Some(resource) = discover_resource_root() {
            let bundled = resource.join(CONFIG_REL);
            if copy_seed_file(&bundled, &user_config) {
                tracing::info!("Seeded settings on first launch → {}", user_config.display());
            }
        }
    }
    std::env::set_var("MEXC_BOT_CONFIG", user_config.display().to_string());
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SeedManifest {
    #[serde(default)]
    settings_sha256: String,
    #[serde(default)]
    db_sha256: String,
    #[serde(default)]
    production_onnx_sha256: String,
    #[serde(default)]
    supervised_onnx_sha256: String,
    #[serde(default)]
    online_model_sha256: String,
    #[serde(default)]
    feature_schema_sha256: String,
}

/// Sync bundled dev settings, training DB, and models when the installer seed manifest changes.
fn apply_bundled_seed(resource_root: &Path, data_dir: &Path) {
    let manifest_path = resource_root.join(BUNDLED_MANIFEST_REL);
    let Some(bundled) = load_seed_manifest(&manifest_path) else {
        apply_bundled_seed_legacy(resource_root, data_dir);
        return;
    };

    let applied = load_applied_manifest(data_dir);
    let mut next_applied = bundled.clone();

    // Settings — always refresh when the installer ships a new dev config.
    if applied
        .as_ref()
        .map(|a| a.settings_sha256.as_str())
        != Some(bundled.settings_sha256.as_str())
    {
        let src = resource_root.join(CONFIG_REL);
        let dst = data_dir.join(CONFIG_REL);
        if copy_seed_file(&src, &dst) {
            tracing::info!("Seeded bundled settings → {}", dst.display());
        } else {
            next_applied.settings_sha256 = applied
                .as_ref()
                .map(|a| a.settings_sha256.clone())
                .unwrap_or_default();
        }
    }

    // Training database — seed on first install, replace placeholders, or refresh our last seed.
    if !bundled.db_sha256.is_empty()
        && applied.as_ref().map(|a| a.db_sha256.as_str()) != Some(bundled.db_sha256.as_str())
    {
        let src = resource_root.join(BUNDLED_DATA_REL).join(SEED_DB_NAME);
        let dst = data_dir.join("data").join(SEED_DB_NAME);
        if database_should_reseed(&dst, &src, applied.as_ref(), &bundled) {
            remove_sqlite_sidecars(&dst);
            if copy_seed_file(&src, &dst) {
                tracing::info!("Seeded bundled database → {}", dst.display());
            } else {
                next_applied.db_sha256 = applied
                    .as_ref()
                    .map(|a| a.db_sha256.clone())
                    .unwrap_or_default();
            }
        } else {
            next_applied.db_sha256 = applied
                .as_ref()
                .map(|a| a.db_sha256.clone())
                .unwrap_or_default();
        }
    }

    // ML models — refresh when the installer ships new weights.
    seed_model_from_manifest(
        resource_root,
        data_dir,
        &applied,
        &bundled,
        &mut next_applied,
        "production.onnx",
        |m| m.production_onnx_sha256.as_str(),
        |m, v| m.production_onnx_sha256 = v,
    );
    seed_model_from_manifest(
        resource_root,
        data_dir,
        &applied,
        &bundled,
        &mut next_applied,
        "supervised.onnx",
        |m| m.supervised_onnx_sha256.as_str(),
        |m, v| m.supervised_onnx_sha256 = v,
    );
    seed_model_from_manifest(
        resource_root,
        data_dir,
        &applied,
        &bundled,
        &mut next_applied,
        "online_model.json",
        |m| m.online_model_sha256.as_str(),
        |m, v| m.online_model_sha256 = v,
    );
    seed_model_from_manifest(
        resource_root,
        data_dir,
        &applied,
        &bundled,
        &mut next_applied,
        "feature_schema.json",
        |m| m.feature_schema_sha256.as_str(),
        |m, v| m.feature_schema_sha256 = v,
    );

    let _ = save_applied_manifest(data_dir, &next_applied);
}

fn seed_model_from_manifest(
    resource_root: &Path,
    data_dir: &Path,
    applied: &Option<SeedManifest>,
    bundled: &SeedManifest,
    next_applied: &mut SeedManifest,
    file_name: &str,
    get_hash: impl Fn(&SeedManifest) -> &str,
    set_hash: impl Fn(&mut SeedManifest, String),
) {
    let bundled_hash = get_hash(bundled);
    if bundled_hash.is_empty() {
        return;
    }
    let applied_hash = applied.as_ref().map(|a| get_hash(a));
    if applied_hash == Some(bundled_hash) {
        return;
    }
    let src = resource_root.join(BUNDLED_MODELS_REL).join(file_name);
    let dst = user_models_dir(data_dir).join(file_name);
    if model_should_reseed(&dst, applied_hash, bundled_hash) && copy_seed_file(&src, &dst) {
        tracing::info!("Seeded bundled model → {}", dst.display());
    } else if let Some(prev) = applied.as_ref() {
        set_hash(next_applied, get_hash(prev).to_string());
    } else {
        set_hash(next_applied, String::new());
    }
}

fn apply_bundled_seed_legacy(resource_root: &Path, data_dir: &Path) {
    let user_config = data_dir.join(CONFIG_REL);
    if !user_config.is_file() {
        let bundled = resource_root.join(CONFIG_REL);
        let _ = copy_seed_file(&bundled, &user_config);
    }
    seed_bundled_database(resource_root, data_dir, None, None);
    seed_bundled_models_legacy(resource_root, data_dir);
}

fn load_seed_manifest(path: &Path) -> Option<SeedManifest> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn load_applied_manifest(data_dir: &Path) -> Option<SeedManifest> {
    load_seed_manifest(&data_dir.join(APPLIED_MANIFEST_NAME))
}

fn save_applied_manifest(data_dir: &Path, manifest: &SeedManifest) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(manifest).unwrap_or_default();
    fs::write(data_dir.join(APPLIED_MANIFEST_NAME), json)
}

fn file_sha256(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(hex::encode(Sha256::digest(bytes)))
}

fn copy_seed_file(src: &Path, dst: &Path) -> bool {
    if !src.is_file() {
        return false;
    }
    if let Some(parent) = dst.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::copy(src, dst).is_ok()
}

fn model_should_reseed(dst: &Path, applied_hash: Option<&str>, bundled_hash: &str) -> bool {
    if !dst.is_file() {
        return true;
    }
    let Some(dst_hash) = file_sha256(dst) else {
        return false;
    };
    if dst_hash == bundled_hash {
        return false;
    }
    applied_hash.is_none_or(|hash| dst_hash == hash)
}

fn database_should_reseed(
    dst: &Path,
    src: &Path,
    applied: Option<&SeedManifest>,
    bundled: &SeedManifest,
) -> bool {
    if !src.is_file() {
        return false;
    }
    if !dst.is_file() {
        return true;
    }
    if database_needs_seed(dst, src) {
        return true;
    }
    let Some(dst_hash) = file_sha256(dst) else {
        return false;
    };
    if dst_hash == bundled.db_sha256 {
        return false;
    }
    if let Some(applied) = applied {
        return dst_hash == applied.db_sha256;
    }
    false
}

/// Copy bundled seed SQLite into the user data folder on first launch.
fn seed_bundled_database(
    resource_root: &Path,
    data_dir: &Path,
    applied: Option<&SeedManifest>,
    bundled: Option<&SeedManifest>,
) {
    let user_data = data_dir.join("data");
    let _ = fs::create_dir_all(&user_data);
    let dst = user_data.join(SEED_DB_NAME);

    let src = resource_root.join(BUNDLED_DATA_REL).join(SEED_DB_NAME);
    if !src.is_file() {
        return;
    }

    let should_seed = if let Some(bundled) = bundled {
        database_should_reseed(&dst, &src, applied, bundled)
    } else if dst.is_file() {
        database_needs_seed(&dst, &src)
    } else {
        true
    };

    if !should_seed {
        return;
    }

    remove_sqlite_sidecars(&dst);
    if copy_seed_file(&src, &dst) {
        tracing::info!(
            "Seeded bundled database {} → {}",
            src.display(),
            dst.display()
        );
    }
}

/// True when the destination is missing or still the empty shell from a prior install.
fn database_needs_seed(dst: &Path, src: &Path) -> bool {
    if !dst.is_file() {
        return true;
    }
    let Ok(dst_len) = fs::metadata(dst).map(|m| m.len()) else {
        return false;
    };
    let Ok(src_len) = fs::metadata(src).map(|m| m.len()) else {
        return false;
    };
    // Placeholder DB from a fresh/empty install is tiny; bundled training DB is much larger.
    src_len > 100_000 && dst_len < 65_536
}

fn remove_sqlite_sidecars(db_path: &Path) {
    let path = db_path.to_string_lossy();
    let _ = fs::remove_file(format!("{path}-wal"));
    let _ = fs::remove_file(format!("{path}-shm"));
}

/// User-writable ML model directory — `data/models/` under the app data root.
fn user_models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(USER_MODELS_REL)
}

/// Move models seeded by older installers (`<app_data>/models/`) into `data/models/`.
fn migrate_legacy_models(data_dir: &Path) {
    let legacy = data_dir.join("models");
    let current = user_models_dir(data_dir);
    let _ = fs::create_dir_all(&current);
    if !legacy.is_dir() {
        return;
    }
    for name in [
        "production.onnx",
        "supervised.onnx",
        "online_model.json",
        "feature_schema.json",
        "production.metrics.json",
    ] {
        let from = legacy.join(name);
        let to = current.join(name);
        if !from.is_file() || to.is_file() {
            continue;
        }
        if fs::copy(&from, &to).is_ok() {
            tracing::info!(
                "Migrated legacy model {} → {}",
                from.display(),
                to.display()
            );
        }
    }
}

/// Copy bundled ONNX / online model files when missing (legacy installers without manifest).
fn seed_bundled_models_legacy(resource_root: &Path, data_dir: &Path) {
    let user_models = user_models_dir(data_dir);
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
