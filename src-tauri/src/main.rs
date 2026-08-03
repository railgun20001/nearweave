// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "windows")]
    if let Some(exit_code) = nearweave_lib::run_installer_helper_from_args() {
        std::process::exit(exit_code);
    }

    nearweave_lib::run()
}
