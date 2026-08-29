#!/usr/bin/env node
// Written by riabuild's `claude_statusline` task — edits here are overwritten.

const fs = require('fs');
const path = require('path');
const { spawn } = require('child_process');

// Marks the status line the way the environment shell marks `PS1` — same word,
// same bold blue. The prompt and the status line are two renderers answering
// one question, so a developer learns the marker once.
//
// Keep this in step with `shell::PROMPT_LABEL`; a test in
// `tasks/claude_statusline.rs` fails if the two drift apart.
//
// Printed even when there is no context data to draw, for the same reason the
// prompt label is unconditional — a marker that comes and goes marks nothing.
const LABEL = '(riabuild)';

// Which repository this is, as `owner/repo`, read out of the checkout the
// session is sitting in.
//
// riabuild stopped being single-repository when the picker landed, and two
// repositories mean two checkouts side by side — same tools, same brokered
// `.env` files, same marker. `(riabuild)` alone answers *which environment is
// this?* and no longer answers *which of my checkouts am I in?*, which is the
// question a developer with `payments` in one window and `ai-builders-hub` in
// another is actually asking.
//
// Read from **git**, not from riabuild's own config, and that is not a
// preference. This script lives in `tools_root()` — one file shared by every
// developer with an account on a server — while `config.json` lives in
// `~/.riabuild-remote/<member-id>/`, a directory this script has no way to
// name. A shared script cannot read per-developer state. Git's answer is also
// the truthful one: it names the repository the cwd is *in*, including a
// checkout riabuild never cloned, rather than the one riabuild last recorded.

// Git's config, without asking the `git` binary for it.
//
// Claude Code re-renders the status line continuously, so this runs far more
// often than a provisioning step ever does. A subprocess per render is a real
// cost to hang on a marker, and `.git/config` is a file — so it is read as one.
// Nothing here writes, and every failure returns null: a status line that
// throws renders as *no status line at all*, which is a worse answer than an
// undecorated label.

// Walks up from `dir` looking for `.git`, and returns the directory holding the
// `config` file — which is not always the `.git` it found.
function gitCommonDir(dir) {
  let at;
  try {
    at = path.resolve(dir);
  } catch {
    return null;
  }
  for (;;) {
    const dot = path.join(at, '.git');
    let stat = null;
    try {
      stat = fs.statSync(dot);
    } catch {
      // Not here. Keep walking.
    }
    if (stat && stat.isDirectory()) return dot;
    // A **linked worktree** has a `.git` *file* naming the real git directory,
    // and that directory holds no `config` of its own — `commondir` points at
    // the one it shares with the main checkout. Worth handling rather than
    // treating as "not a repository": every branch of riabuild's own work
    // happens in one, under `.claude/worktrees/`, which is exactly where a
    // developer most needs to be told which repository they are in.
    if (stat && stat.isFile()) {
      let named;
      try {
        named = /^gitdir:\s*(.+)$/m.exec(fs.readFileSync(dot, 'utf8'));
      } catch {
        return null;
      }
      if (!named) return null;
      const gitdir = path.resolve(at, named[1].trim());
      try {
        const shared = fs.readFileSync(path.join(gitdir, 'commondir'), 'utf8');
        return path.resolve(gitdir, shared.trim());
      } catch {
        // No `commondir`: a `.git` file that is not a linked worktree — a
        // submodule is the usual one — so the directory it names is its own.
        return gitdir;
      }
    }
    const up = path.dirname(at);
    if (up === at) return null; // reached the filesystem root
    at = up;
  }
}

// `url` from the `[remote "origin"]` section, by walking the INI rather than
// parsing it: this needs one value out of one section, and every other key in
// the file is somebody else's business.
function originUrl(gitdir) {
  let text;
  try {
    text = fs.readFileSync(path.join(gitdir, 'config'), 'utf8');
  } catch {
    return null;
  }
  let inOrigin = false;
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (line.startsWith('#') || line.startsWith(';')) continue;
    const section = /^\[([^\]]+)\]/.exec(line);
    if (section) {
      inOrigin = /^remote\s+"origin"$/.test(section[1].trim());
      continue;
    }
    if (!inOrigin) continue;
    const url = /^url\s*=\s*(.*)$/.exec(line);
    if (url) return url[1].trim() || null;
  }
  return null;
}

// `owner/repo` out of any spelling git records a remote in — `git@host:o/r.git`,
// `https://host/o/r.git`, `ssh://git@host/o/r`. The last two segments are the
// answer in all of them, which is why this takes the tail rather than trying to
// know the shape of a URL.
function slugOf(url) {
  const trimmed = url.replace(/\.git$/, '').replace(/[/]+$/, '');
  const tail = /([^/:]+)\/([^/:]+)$/.exec(trimmed);
  return tail ? `${tail[1]}/${tail[2]}` : '';
}

function repoOf(dir) {
  if (!dir) return '';
  const gitdir = gitCommonDir(dir);
  if (!gitdir) return '';
  const url = originUrl(gitdir);
  // A checkout with no `origin` is a repository riabuild has nothing to say
  // about — better an undecorated marker than a guess from the directory name.
  return url ? slugOf(url) : '';
}

// The repository goes *inside* the parentheses — `(riabuild · Clubria/payments)`
// — so there is still one marker to learn rather than a marker with a second
// thing sitting next to it. Spliced into `LABEL` instead of spelled out again,
// so the word the prompt and the status line share stays in one place.
function marker(repo) {
  const inner = repo ? `${LABEL.slice(0, -1)} · ${repo})` : LABEL;
  return `\x1b[1;34m${inner}\x1b[0m`;
}

// How full the context window is: `█████░░░░░ 54%`, coloured green → yellow →
// orange → blinking red 💀. Returns '' when Claude Code sends no window data.
function contextBar(payload) {
  const remaining = payload.context_window?.remaining_percentage; // 0-100 left
  if (remaining == null) return '';

  // Claude Code reserves a buffer for auto-compaction (~16.5% by default).
  // Measure the USABLE window so the bar reads 100% when compaction kicks in.
  const totalCtx = payload.context_window?.total_tokens || 1_000_000;
  const acw = parseInt(process.env.CLAUDE_CODE_AUTO_COMPACT_WINDOW || '0', 10);
  const bufferPct = acw > 0
    ? Math.min(100, Math.max(0, (1 - acw / totalCtx) * 100))
    : 16.5;

  const usableRemaining = Math.max(0, ((remaining - bufferPct) / (100 - bufferPct)) * 100);
  const used = Math.max(0, Math.min(100, Math.round(100 - usableRemaining)));

  // 10-segment bar: filled blocks + empty blocks.
  const filled = Math.floor(used / 10);
  const bar = '█'.repeat(filled) + '░'.repeat(10 - filled); // █ / ░

  // Color by how full it is (ANSI): green <50, yellow <65, orange <80, blinking red >=80.
  if (used < 50)      return ` \x1b[32m${bar} ${used}%\x1b[0m`;
  else if (used < 65) return ` \x1b[33m${bar} ${used}%\x1b[0m`;
  else if (used < 80) return ` \x1b[38;5;208m${bar} ${used}%\x1b[0m`;
  else                return ` \x1b[5;31m\u{1F480} ${bar} ${used}%\x1b[0m`; // 💀 blinking
}

// Where the session is *now*, not where it started: a developer who has cd'd
// into a second checkout is in that repository, and `project_dir` would still
// name the first. `process.cwd()` is the fallback for a Claude Code that sends
// neither, since it runs this script in the session's directory anyway.
function cwdOf(payload) {
  return payload.workspace?.current_dir || payload.cwd || process.cwd();
}


// How often starting a flush is worth it. This script runs about once per
// assistant message, which is far more often than a usage dashboard needs, so
// the spool is *sent* at most once a minute and every other render only appends.
const FLUSH_EVERY_MS = 60_000;

// One usage sample, in the shape `POST /api/v1/usage` takes.
//
// Only fields Claude Code documents as **cumulative for the session** are
// carried, because the server merges samples by taking the larger of what it
// holds and what arrives. A per-call figure merged that way would report the
// largest single request rather than the session — a number that means nothing
// and looks like one that does.
//
// **No token count is collected, and that is deliberate.**
// `context_window.total_input_tokens` reads like a session total and is
// documented as the tokens *currently in the context window*: `0` before the
// first response, and smaller again after every `/compact`. Merged by maximum
// it would report the largest the context ever grew, under a column heading
// that said "tokens". `cost.total_cost_usd` is the only cumulative measure of
// volume the status line offers, so it is the one taken.
//
// Nothing about *what* the developer was doing is collected: no prompt, no file
// path, and not the repository — which this script has in hand for the marker
// and deliberately does not send.
function sample(payload) {
  const cost = payload.cost ?? {};
  const limits = payload.rate_limits ?? {};
  const out = {
    harness: 'claude',
    sessionId: payload.session_id,
    model: payload.model?.id,
    costUsd: cost.total_cost_usd,
    durationMs: cost.total_duration_ms,
    apiDurationMs: cost.total_api_duration_ms,
    linesAdded: cost.total_lines_added,
    linesRemoved: cost.total_lines_removed,
    fiveHourPct: limits.five_hour?.used_percentage,
    fiveHourResetsAt: limits.five_hour?.resets_at,
    sevenDayPct: limits.seven_day?.used_percentage,
    sevenDayResetsAt: limits.seven_day?.resets_at,
  };
  // An absent cost means unreported, never free, and the server tells the two
  // apart — so a key with no value is dropped rather than sent as null.
  for (const key of Object.keys(out)) {
    if (out[key] == null) delete out[key];
  }
  return out;
}

// Appends a sample and, at most once a minute, starts a flush.
//
// Every failure is swallowed by the caller. This is on the render path of an
// interactive session, and a provisioner that breaks a developer's status line
// because a dashboard is unreachable has turned a usage tracker into an outage.
function collect(payload) {
  // Set by the Claude launcher, and **only for an account the developer marked
  // as work** — see `riabuild claude track`. An untracked account is handed no
  // path, so this returns before writing anything and a personal subscription
  // leaves no trace at all.
  const spool = process.env.RIABUILD_USAGE_SPOOL;
  if (!spool || !payload.session_id) return;

  // The spool is `<root>/usage/<account-uuid>.ndjson`, so the account names
  // itself and nothing has to be passed twice.
  const accountId = path.basename(spool, '.ndjson');
  fs.mkdirSync(path.dirname(spool), { recursive: true });
  fs.appendFileSync(spool, JSON.stringify({ ...sample(payload), accountId }) + '\n');

  // The marker's mtime is when a flush was last *attempted*, not when one last
  // succeeded. A laptop that cannot reach riabuild-web then retries once a
  // minute and no more; moving it only on success would spawn a process on
  // every render for as long as the dashboard was down.
  const marker = path.join(path.dirname(spool), 'flushed');
  let due;
  try {
    due = Date.now() - fs.statSync(marker).mtimeMs >= FLUSH_EVERY_MS;
  } catch {
    due = true; // no marker yet: this machine has never flushed.
  }
  if (!due) return;

  // Absolute, from the launcher, because `~/.riabuild/bin` is the one directory
  // riabuild does not put itself in — see `no_shim_looks_riabuild_up_on_the_path`.
  // `RIABUILD_SELF` and not `RIABUILD_BIN`, which e2e and CI already use to name
  // the binary under test. Without it the sample still lands in the spool and
  // the next `riabuild` run sends it; only the one-a-minute cadence is lost.
  const riabuild = process.env.RIABUILD_SELF;
  if (!riabuild) return;

  // Detached, with its output thrown away. Claude Code kills a status line
  // script a newer render supersedes, and a flush in this process group would
  // die with it — mid-POST, having already taken the lock.
  spawn(riabuild, ['internal', 'usage-flush'], {
    detached: true,
    stdio: 'ignore',
  }).unref();
}

let input = '';
process.stdin.on('data', (c) => (input += c));
process.stdin.on('end', () => {
  let label = marker('');
  let bar = '';
  let payload = null;
  try {
    payload = JSON.parse(input || '{}');
    label = marker(repoOf(cwdOf(payload)));
    bar = contextBar(payload);
  } catch {
    // Silent fail: a broken bar still leaves a labelled status line.
  }

  // After the marker and the bar, and in its own `try`, so that nothing about
  // collecting usage can cost a developer the status line they asked for.
  try {
    if (payload) collect(payload);
  } catch {
    // Silent fail: see `collect`.
  }

  process.stdout.write(label + bar);
});
