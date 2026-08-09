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
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use clap::{Parser, Subcommand};

use uncompose_compare::cache::{Cache, DEFAULT_CACHE_MAX_BYTES};
use uncompose_compare::project::{uncompose_project_on_path, Manifest, Resolved};
use uncompose_compare::record::Recorder;
use uncompose_compare::server::{bind, serve, session_token};
use uncompose_compare::session::{Lane, Session};

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
    /// refused, never overwritten. Conflicts with `--project` (project records'
    /// destination is the handover's, M5 slice 5).
    #[arg(long, value_name = "PATH", conflicts_with = "project")]
    out: Option<PathBuf>,

    /// Project mode (spec #42): resolve the two positionals as manifest refs
    /// against `<DIR>/uncompose.project.json` instead of as file paths. A ref is
    /// an asset slug, or `<name>@<derivation>`. The candidates' shared source
    /// becomes the SRC lane unless `--exclude-source` is passed.
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,

    /// The shared source for the SRC lane in bare-file mode (spec #42): a third,
    /// always-identified lane that joins the sample-locked graph and the loudness
    /// match group. In project mode the source is auto-resolved instead, so this
    /// conflicts with `--project`.
    #[arg(long, value_name = "PATH", conflicts_with = "project")]
    source: Option<PathBuf>,

    /// Project mode: omit the SRC lane even when the candidates share a source.
    #[arg(long, requires = "project", conflicts_with = "source")]
    exclude_source: bool,

    /// Match playback loudness: measure ITU-R BS.1770 integrated loudness per
    /// candidate at load and attenuate the louder lane down to the quietest
    /// (gain-only, never boost). Off by default — faithful as-is playback is the
    /// baseline (#66).
    #[arg(long)]
    loudness_match: bool,

    /// Blind session: shuffle the label↔file assignment by an OS coin flip at
    /// load and conceal every identifying detail (name, path, hash, size) from
    /// the served surface, so a preference is judged without knowing which file
    /// is which. Refuses identical content or a duration/rate/channel mismatch
    /// before binding. The written record still carries full identities (#28).
    #[arg(long)]
    blind: bool,
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

/// Resolve the two operands (and any SRC lane) into loadable lanes. Bare-file
/// mode takes the operands as paths and `--source` as the optional SRC lane;
/// project mode (spec #42) reads the manifest, resolves each operand as a ref,
/// auto-resolves the shared source, and pre-flights the project tool — every
/// failure here happens before the server binds.
fn resolve_lanes(
    cli: &Cli,
    a: &Path,
    b: &Path,
) -> Result<(Lane, Lane, Option<Lane>), Box<dyn std::error::Error + Send + Sync>> {
    let Some(project_dir) = &cli.project else {
        // Bare-file mode: the operands are paths, `--source` is the SRC lane.
        return Ok((
            Lane::bare(a.to_path_buf()),
            Lane::bare(b.to_path_buf()),
            cli.source.clone().map(Lane::bare),
        ));
    };

    // Project mode. Pre-flight so a session that cannot be registered never
    // starts: the manifest must parse, both refs must resolve, and the project
    // tool must be installed.
    let manifest = Manifest::load(project_dir)?;
    if !uncompose_project_on_path() {
        return Err("uncompose-project is not on PATH; install it \
             (`pip install uncompose-project`) before launching a project session"
            .into());
    }

    let a_tok = a.to_str().ok_or("the first ref is not valid UTF-8")?;
    let b_tok = b.to_str().ok_or("the second ref is not valid UTF-8")?;
    let ra = manifest.resolve(a_tok)?;
    let rb = manifest.resolve(b_tok)?;

    let lane_for = |resolved: &Resolved| Lane {
        path: resolved.path(&manifest),
        expected_sha256: Some(resolved.asset.sha256.clone()),
        asset: Some(resolved.asset.id.clone()),
        project: Some(manifest.id.clone()),
    };
    let a_lane = lane_for(&ra);
    let b_lane = lane_for(&rb);

    // The SRC lane: the candidates' shared source, unless opted out. Sharing none
    // is a stated absence, not an error.
    let source_lane = if cli.exclude_source {
        None
    } else {
        manifest.shared_source(&ra, &rb)?.map(|asset| Lane {
            path: project_dir.join(&asset.file),
            expected_sha256: Some(asset.sha256.clone()),
            // SRC is a playback lane, not a comparison candidate — it never enters
            // the record's `candidates[]`, so it carries no recorded asset/project.
            asset: None,
            project: None,
        })
    };

    Ok((a_lane, b_lane, source_lane))
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

    // The default form needs exactly two candidate operands (paths, or manifest
    // refs in project mode). clap already rejects a third positional as
    // "unexpected"; a zero/one-operand invocation lands here.
    let (a, b) = match (&cli.a, &cli.b) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            return Err("two audio files are required\n\n\
                 Usage: uncompose-compare <A> <B>"
                .into())
        }
    };

    // Resolve the two operands into lanes (plus an optional SRC lane): straight
    // from disk in bare-file mode, or from the project manifest in project mode.
    let (a_lane, b_lane, source_lane) = resolve_lanes(cli, a, b)?;

    // Load both candidates before binding: a bad invocation must fail with a
    // clear message and a non-zero exit, never a running server. Loading also
    // transcodes each input into a cached playback proxy (#74).
    let session = Session::load(
        a_lane,
        b_lane,
        source_lane,
        &cache,
        cli.loudness_match,
        cli.blind,
    )?;

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
