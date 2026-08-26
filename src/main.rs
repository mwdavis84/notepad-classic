#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod dialogs;
mod file;

#[cfg(windows)]
fn main() {
    if let Err(message) = app::run() {
        dialogs::show_error(None, "Notepad Classic", &message);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Notepad Classic runs on Windows only.");
}
