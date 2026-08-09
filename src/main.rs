//! The `uncompose-compare` command: parse arguments, wire the core together,
//! format output.
//!
//! `uncompose-compare <a> <b>` loads two audio files as candidates A and B (in
//! argument order), hashing and decoding each at startup, then serves the
//! embedded UI over the guarded loopback server plus the `/session`,
//! `/audio/<sha256>`, and `/record` endpoints (see `lib.rs` for the core).
//!
//! Bad invocations fail before the server ever binds, with a clear message and
//! a non-zero exit: wrong argument count (clap), a missing or unreadable file,
//! or an undecodable format.

use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

use clap::{Parser, Subcommand};

use uncompose_compare::cache::{Cache, DEFAULT_CACHE_MAX_BYTES};
use uncompose_compare::record::Recorder;
use uncompose_compare::server::{bind, serve, session_token};
use uncompose_compare::session::Session;

/// uncompose-compare — load two audio files and open the listening workbench.
#[derive(Parser)]
#[command(name = "uncompose-compare", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Candidate A: the first audio file to compare.
    a: Option<PathBuf>,
    /// Candidate B: the second audio file to compare.
    b: Option<PathBuf>,

    /// Bind this loopback port instead of an ephemeral one (per #72).
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,

    /// Prune the proxy cache to at most this many bytes (LRU, at startup only).
    #[arg(long, value_name = "BYTES", default_value_t = DEFAULT_CACHE_MAX_BYTES)]
    cache_max_bytes: u64,

    /// Write the concluded comparison record here instead of the default
    /// `<ULID>.json` in the invoking directory. An existing destination is
    /// refused, never overwritten.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,

    /// Match playback loudness: measure ITU-R BS.1770 integrated loudness per
    /// candidate at load and attenuate the louder lane down to the quietest
    /// (gain-only, never boost). Off by default — faithful as-is playback is the
    /// baseline (#66).
    #[arg(long)]
    loudness_match: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Proxy-cache maintenance.
    Cache {
        #[command(subcommand)]
        action: CacheCommand,
    },
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Delete every cached playback proxy and report what was removed.
    Clear,
}

fn main() {
    let cli = Cli::parse();

    if let Err(err) = run(&cli) {
        eprintln!("uncompose-compare: {err}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cache = Cache::new(cli.cache_max_bytes)?;

    // `cache clear` is a maintenance path that never binds a server.
    if let Some(Command::Cache {
        action: CacheCommand::Clear,
    }) = &cli.command
    {
        let (files, bytes) = cache.clear()?;
        if files == 0 {
            println!("cache already empty ({})", cache.dir.display());
        } else {
            println!(
                "cleared {files} proxy file{} ({bytes} bytes) from {}",
                if files == 1 { "" } else { "s" },
                cache.dir.display(),
            );
        }
        return Ok(());
    }

    // The default form needs exactly two candidate paths. clap already rejects a
    // third positional as "unexpected"; a zero/one-file invocation lands here.
    let (a, b) = match (&cli.a, &cli.b) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            return Err("two audio files are required\n\n\
                 Usage: uncompose-compare <A> <B>"
                .into())
        }
    };

    // Load both candidates before binding: a bad invocation must fail with a
    // clear message and a non-zero exit, never a running server. Loading also
    // transcodes each input into a cached playback proxy (#74).
    let session = Session::load(a, b, &cache, cli.loudness_match)?;

    // Prune the cache once, at startup, never mid-session (#72). The proxies
    // this run just wrote/reused carry the freshest access time, so an LRU prune
    // evicts stale entries from earlier sessions before it ever touches ours.
    cache.prune()?;

    // Loopback bind: `--port` pins the port, otherwise the OS hands us a free
    // one. Either way the server is never exposed beyond this machine.
    let (server, port) = bind(cli.port.unwrap_or(0))?;

    // Per-session token: the printed URL is the only thing that carries it, so
    // knowing the port alone is not enough to talk to the server.
    let token = session_token()?;

    // The record writer: the default destination is the invoking directory
    // (named by the record's ULID at conclude time), overridden by `--out`. The
    // session's start is the record's `created_at`; `completed_at` is stamped at
    // conclude. Built before serving so an unreadable cwd or a broken embedded
    // schema fails before a listening session, not after it. Whether the
    // destination is writable is settled at conclude (ADR-0002), where the
    // `create_new` reservation is the existence check.
    let recorder = Recorder::new(cli.out.clone(), std::env::current_dir()?, SystemTime::now())?;

    let url = format!("http://127.0.0.1:{port}/?token={token}");
    println!("{url}");
    std::io::stdout().flush()?;

    for request in server.incoming_requests() {
        serve(request, &token, &session, &recorder);
    }

    Ok(())
}
