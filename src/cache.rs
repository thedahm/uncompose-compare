//! The XDG content-hash proxy cache (#74).
//!
//! Proxies are named `<source-sha256>.<ext>`, so a source's proxy is found by
//! hash alone — no request path ever reaches the filesystem. A second run
//! against unchanged files reuses them and re-transcodes nothing; the cache is
//! pruned oldest-accessed to a (configurable) cap at startup only, and
//! `uncompose-compare cache clear` wipes it.

use std::fs::{File, FileTimes};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::audio::{encode, Container, DecodedPcm};

/// The default proxy-cache cap: 2 GiB, per #72. Overridable with
/// `--cache-max-bytes` so the LRU prune is tunable (and testable).
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// A cached playback proxy for one source: the on-disk file the audio endpoint
/// streams, and the container it was encoded in (so the response's Content-Type
/// is honest).
pub struct Proxy {
    pub path: PathBuf,
    pub container: Container,
}

/// The proxy cache rooted at one directory, pruned LRU (by access time) to
/// `max_bytes` at startup only.
pub struct Cache {
    pub dir: PathBuf,
    max_bytes: u64,
}

impl Cache {
    /// Locate the cache under `$XDG_CACHE_HOME/uncompose-compare` (falling back
    /// to `$HOME/.cache/...`, Linux-only per the v0.1 scope) and ensure it
    /// exists.
    pub fn new(max_bytes: u64) -> Result<Cache, Box<dyn std::error::Error + Send + Sync>> {
        let base = match std::env::var_os("XDG_CACHE_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x),
            _ => {
                let home = std::env::var_os("HOME").ok_or("neither XDG_CACHE_HOME nor HOME set")?;
                PathBuf::from(home).join(".cache")
            }
        };
        let dir = base.join("uncompose-compare");
        std::fs::create_dir_all(&dir)?;
        Ok(Cache { dir, max_bytes })
    }

    /// Return the proxy for `hash`, transcoding it into the cache if absent.
    /// A cache hit rewrites nothing — it only bumps the file's access time so the
    /// LRU prune keeps files this session used — so re-running on unchanged
    /// inputs performs no re-transcode.
    pub fn ensure_proxy(&self, hash: &str, pcm: &DecodedPcm) -> Result<Proxy, String> {
        // The container choice is a pure function of the source, so it is stable
        // across runs: the same source always maps to the same cache filename.
        let container = Container::for_channels(pcm.channels);
        let path = self.dir.join(format!("{hash}.{}", container.ext()));

        if path.exists() {
            // Mark the reuse without touching the file's contents or mtime: only
            // the access time moves, which is what the LRU prune orders by.
            let _ = File::options()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_times(FileTimes::new().set_accessed(SystemTime::now())));
            return Ok(Proxy { path, container });
        }

        let encoded = encode(pcm, container)?;

        // Write to a private temp sibling then rename, so a crash mid-encode
        // never leaves a half-written proxy the next run would trust. The temp
        // name is unique per process (the final name is content-addressed, so
        // two runs transcoding the same source race only to replace it with
        // identical bytes).
        let tmp = self.dir.join(format!(
            "{hash}.{}.{}.tmp",
            container.ext(),
            std::process::id()
        ));
        std::fs::write(&tmp, &encoded).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(Proxy { path, container })
    }

    /// Prune the cache to `max_bytes`, evicting least-recently-accessed proxies
    /// first. Called once at startup; never mid-session (#72).
    pub fn prune(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut entries: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        let mut total: u64 = 0;
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let meta = match entry.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let atime = meta.accessed().unwrap_or(SystemTime::UNIX_EPOCH);
            total += meta.len();
            entries.push((entry.path(), meta.len(), atime));
        }

        if total <= self.max_bytes {
            return Ok(());
        }

        // Oldest access first: those are the entries the prune sheds.
        entries.sort_by_key(|(_, _, atime)| *atime);
        for (path, len, _) in entries {
            if total <= self.max_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
        Ok(())
    }

    /// Delete every proxy in the cache, returning (file count, byte total)
    /// removed so `cache clear` can report what it did.
    pub fn clear(&self) -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>> {
        let mut files = 0u64;
        let mut bytes = 0u64;
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let meta = match entry.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let len = meta.len();
            if std::fs::remove_file(entry.path()).is_ok() {
                files += 1;
                bytes += len;
            }
        }
        Ok((files, bytes))
    }
}
