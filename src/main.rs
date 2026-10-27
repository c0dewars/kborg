#![forbid(unsafe_code)]
fn main() {
    let result = kborg::cli::run();
    if let Some(warning) = kborg::audit_sink::take_warning() {
        eprintln!("KBorg: {}", kborg::safety::safe_text(&warning, 1200));
    }
    if let Err(error) = result {
        eprintln!("KBorg: {}", kborg::safety::safe_text(&error, 1200));
        std::process::exit(1);
    }
}
