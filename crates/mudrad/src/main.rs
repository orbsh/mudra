//! mudrad binary entry: `mudrad run` (foreground, the only supported
//! action in R1 — start/stop supervision is systemd's job on NixOS).

fn main() {
    let act = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    if act != "run" {
        eprintln!("mudrad: only 'run' is supported (foreground); exit 2");
        std::process::exit(2);
    }
    // A runtime that cannot start is a boot failure the supervisor must see:
    // report it and exit non-zero instead of panicking inside a task-less process.
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("mudrad: cannot start the tokio runtime: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = rt.block_on(mudrad::daemon::run()) {
        eprintln!("[mudrad] {e}");
        std::process::exit(1);
    }
}
