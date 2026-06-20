mod arena;
mod config;
mod crypto;
mod dns;
mod gzip;
mod http;
mod inflate;
mod json;
mod monitor;
mod protocol;
mod proxy;
mod server;
mod tls;
mod ws;

#[cfg(feature = "terminal")]
mod terminal;

#[cfg(feature = "self-update")]
mod update;

use std::env;
use std::process;

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut config = config::Config::default();

    if let Err(e) = config::parse_args(&mut config, &args) {
        eprintln!("Error parsing arguments: {:?}", e);
        eprintln!("Usage: komari-agent --endpoint <url> --token <token> [options]");
        process::exit(1);
    }

    config::load_env(&mut config);

    // Layer 3 (highest priority): JSON config file overrides flags + env.
    // Matches Go's load order (flag → env → --config file).
    if !config.config_file.is_empty() {
        let path = config.config_file.clone();
        if let Err(e) = config::load_json_config(&mut config, &path) {
            eprintln!("Error loading config file '{}': {:?}", path, e);
            process::exit(1);
        }
    }

    if let Err(e) = config::validate(&config) {
        eprintln!("Configuration error: {:?}", e);
        process::exit(1);
    }

    // Self-update check (feature-gated). Runs once at startup when auto-update
    // is enabled; a successful replacement re-execs and the new binary takes
    // over. Failure is non-fatal — we proceed with the current binary.
    #[cfg(feature = "self-update")]
    if !config.disable_auto_update {
        match update::check_and_update(env!("CARGO_PKG_VERSION"), &config) {
            Ok(true) => {
                // Updated and (on Unix) re-execed; on Windows the new binary
                // is staged for next launch via MoveFileEx(DELAY_UNTIL_REBOOT).
                eprintln!("[komari] update applied, continuing with current process");
            }
            Ok(false) => { /* up to date */ }
            Err(e) => {
                eprintln!("[komari] WARN: self-update check failed: {:?}", e);
            }
        }
    }

    server::run(&config);
}
