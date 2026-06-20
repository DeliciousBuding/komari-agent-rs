mod arena;
mod config;
mod crypto;
mod dns;
mod http;
mod json;
mod monitor;
mod protocol;
mod server;
mod tls;
mod ws;

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
