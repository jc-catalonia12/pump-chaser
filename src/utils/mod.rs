pub mod alerts;
pub mod paths;
pub mod secrets;
pub mod telegram_bot;
pub mod time;

pub use alerts::Alerter;
pub use paths::{
    append_startup_log, app_data_dir, app_icon_path, candidate_web_dirs, discover_project_root,
    discover_resource_root, init_runtime_paths, init_runtime_paths_with_resource,
    init_working_directory, is_packaged_install, normalize_config_paths, resolve_data_path,
    web_assets_dir,
};
pub use secrets::{load_secrets, merge_secrets_update, save_secrets, UserSecrets};
pub use telegram_bot::spawn_command_poller;
pub use time::utc_now_rfc3339;
