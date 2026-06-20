mod arena;
mod config;
mod crypto;
mod dns;
mod gzip;
mod http;
mod json;
mod monitor;
mod proxy;
mod protocol;
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

    if let Err(e) = config::validate(&config) {
        eprintln!("Configuration error: {:?}", e);
        process::exit(1);
    }

    server::run(&config);
}
