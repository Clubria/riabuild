//! Writing a newly-trusted key into riabuild's own `known_hosts`.

use std::path::Path;

use anyhow::Result;
use riabuild_paths::Paths;

use crate::identity::ensure_private_dir;

/// Appends a newly-trusted host key to riabuild's own `known_hosts`,
/// creating its directory (`0700`) if needed. Shared by the `accept` and
/// interactive paths so there is exactly one place that writes this file.
///
/// **Append-only, no read-modify-write.** A prior version re-read
/// `known_hosts` right before composing a full rewrite, which closed the
/// original stale-snapshot window but still let two genuinely concurrent
/// `pin` calls each read before either wrote (and its temp-file name,
/// process-id-only, collided between them regardless). `trust_host` only
/// ever calls this for a host with no existing entry — its `already` check
/// returns earlier otherwise — so there is nothing here to *replace*, only
/// to add, and `O_APPEND` has no read step to go stale: the kernel
/// atomically extends the file and places the write at the new end, so two
/// concurrent appenders on one local filesystem cannot overwrite each
/// other's bytes. (This assumes a local filesystem, true here under
/// `~/.riabuild`; `O_APPEND` is not atomic across clients on NFS.)
///
/// The one thing append can get wrong is gluing onto a line missing its
/// trailing `\n` (a hand-edited file) — guarded by leading with a newline
/// when the file already has bytes. A race on that check costs at most one
/// redundant blank line, which `ssh` ignores, never lost or corrupted data.
pub(super) async fn pin(paths: &dyn Paths, known_hosts: &Path, keys: &str) -> Result<()> {
    ensure_private_dir(&paths.ssh_dir()).await?;
    let has_content = tokio::fs::metadata(known_hosts)
        .await
        .map(|meta| meta.len() > 0)
        .unwrap_or(false);
    let mut entry = String::new();
    if has_content {
        entry.push('\n');
    }
    entry.push_str(keys);
    entry.push('\n');
    append(known_hosts, entry.as_bytes()).await
}

/// Opens `path` for append (creating it if needed) and writes `bytes`,
/// flushed before returning — `write_all` alone only queues the bytes for a
/// blocking-pool task to actually write, the same gap `keychain/file.rs`'s
/// `write_private_token` was fixed for.
async fn append(path: &Path, bytes: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(bytes).await?;
    file.flush().await?;
    Ok(())
}
