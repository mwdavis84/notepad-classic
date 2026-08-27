#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod dialogs;
mod file;
#[cfg(windows)]
mod localization;
#[cfg(windows)]
mod printing;
#[cfg(test)]
mod resource_catalog;

#[cfg(windows)]
fn main() {
    if let Err(message) = app::run() {
        let title = localization::text(localization::ids::IDS_APP_NAME);
        let title = String::from_utf16_lossy(localization::without_trailing_nul(&title));
        dialogs::show_error(None, &title, &message);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Notepad Classic runs on Windows only.");
}
