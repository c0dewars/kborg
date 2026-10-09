#![forbid(unsafe_code)]
fn main() {
    if let Err(error) = k8borg::cli::run() {
        eprintln!("K8Borg: {}", k8borg::safety::safe_text(&error, 1200));
        std::process::exit(1);
    }
}
