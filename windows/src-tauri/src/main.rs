// ACS for Windows — thin entry point; everything lives in the library crate
// so integration tests can drive the helper transport directly.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    acs_windows::run();
}
