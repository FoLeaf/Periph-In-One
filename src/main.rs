#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
slint::include_modules!();

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod platform;

#[cfg(windows)]
fn main() {
    if let Some(result) = platform::update::internal_entry() {
        if let Err(error) = result {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = app::run() {
        platform::error_dialog(&error);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Periph In One 面向 Windows 10/11。核心测试可在本机运行：cargo test --lib --tests");
}
