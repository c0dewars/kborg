#![forbid(unsafe_code)]
fn main() {
    if let Err(error) = kborg::cli::run() {
        eprintln!("KBorg: {}", kborg::safety::safe_text(&error, 1200));
        std::process::exit(1);
    }
}
