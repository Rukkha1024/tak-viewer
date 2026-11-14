//! `tak_dump --dump <file.tak> <out.json>`: same output as `tak_viewer --dump`, without the GUI.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    std::process::exit(tak_core::dump::run(&args[1..]));
}
