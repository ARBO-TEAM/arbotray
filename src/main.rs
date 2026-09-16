// Release builds are windows-subsystem so launching from Explorer never
// flashes a console. Debug builds keep the console for logging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    std::process::exit(arbotray::app::run());
}
