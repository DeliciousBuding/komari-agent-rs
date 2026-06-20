// komari-agent-rs — featherweight Komari monitoring agent.
//
// Foundation phase (M1): all spine modules declared but not yet wired.
// `#[allow(dead_code)]` on each module is intentional — these are
// foundation modules whose public API will be consumed in later phases.

#[allow(dead_code)]
mod arena;

#[allow(dead_code)]
mod config;

#[allow(dead_code)]
mod crypto;

#[allow(dead_code)]
mod json;

#[allow(dead_code)]
mod protocol;

fn main() {
    // M1: Foundation + Handshake — entry point placeholder
}
