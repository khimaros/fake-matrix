// fake-matrix: a standalone fake matrix homeserver for testing matrix bots and
// bridges without a real homeserver. see README.md and DESIGN.md.

mod admin;
mod config;
mod matrix;
mod response;
mod router;
mod server;
mod state;

/// trace to stderr, on unless FAKE_MATRIX_LOG=0. a test harness that captures stderr then keeps
/// the SERVER's account of an exchange, which is the half you cannot reconstruct from a client
/// log: whether an injected event was delivered, to whom, and why it was filtered out. deliberately
/// an eprintln! rather than a logging crate -- this binary has no dependencies and should keep none.
pub fn log(args: std::fmt::Arguments<'_>) {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    if *ON.get_or_init(|| std::env::var("FAKE_MATRIX_LOG").as_deref() != Ok("0")) {
        eprintln!("fake-matrix: {args}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", config::usage());
        return;
    }
    match config::parse_args(args) {
        Ok(config) => server::serve(config),
        Err(e) => {
            eprintln!("fake-matrix: {e}");
            std::process::exit(2);
        }
    }
}
