//! Spike 1: embedded hello skeleton.
//!
//! Serves the Vite/React stub bundle — embedded into this binary via
//! rust-embed — over a loopback-only HTTP server on an ephemeral port. This is
//! the composition the Track C spike (spec #1) exists to prove: no loose asset
//! directory at runtime, a single self-contained artifact.
//!
//! Scope for this issue (#2) is deliberately narrow: bind, print the URL, serve
//! the embedded page, and answer `--version`/`--help`. The fuller #72 server
//! contract (session token, Host check) lands in a follow-up issue.

use std::io::Write;

use clap::Parser;
use rust_embed::RustEmbed;
use tiny_http::{Header, Response, Server};

/// The Vite/React stub bundle, embedded at compile time. `build.rs` guarantees
/// the folder is present and non-empty, so a build that reaches here has assets.
#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;

/// uncompose-compare — serve the embedded UI over loopback.
#[derive(Parser)]
#[command(name = "uncompose-compare", version, about, long_about = None)]
struct Cli {}

fn main() {
    Cli::parse();

    match run() {
        Ok(()) => {}
        Err(err) => {
            eprintln!("uncompose-compare: {err}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Loopback bind on an ephemeral port: the OS hands us a free port and we
    // never expose the server beyond this machine.
    let server = Server::http("127.0.0.1:0")?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or("server bound to a non-IP address")?
        .port();

    let url = format!("http://127.0.0.1:{port}/");
    println!("{url}");
    std::io::stdout().flush()?;

    for request in server.incoming_requests() {
        serve(request);
    }

    Ok(())
}

/// Resolve the request against the embedded bundle and reply.
fn serve(request: tiny_http::Request) {
    // Strip any query string, then map "/" to the SPA entry point.
    let raw = request.url();
    let path = raw.split(['?', '#']).next().unwrap_or("/");
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    let response = match Assets::get(path) {
        Some(file) => {
            let content_type = file.metadata.mimetype();
            Response::from_data(file.data.into_owned())
                .with_header(header("Content-Type", content_type))
                // Privacy posture from the first served byte (#72): never cache.
                .with_header(header("Cache-Control", "no-store"))
        }
        None => Response::from_string("not found")
            .with_status_code(404)
            .with_header(header("Cache-Control", "no-store")),
    };

    // A broken client connection is not our problem to recover from.
    let _ = request.respond(response);
}

fn header(field: &str, value: &str) -> Header {
    Header::from_bytes(field.as_bytes(), value.as_bytes())
        .expect("static header field/value are always valid")
}
