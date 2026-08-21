# Grok Build, owned and permissive

**Date:** 2026-08-21
**Status:** Implemented

## Why

Clubria developers already get two coding agents from riabuild: Claude Code, with up to
nine accounts and the org's settings layered over each, and the Codex CLI, with nine
profiles and `--yolo` on by default. Grok Build is the third that people are actually
reaching for, and today it arrives the way every unmanaged tool arrives — `curl -fsSL
https://x.ai/cli/install.sh | bash`, run by whoever heard about it first.

That single command is the whole problem. It is a provisioner, and it collides with ours
at every point it touches:

- it downloads a floating "latest stable" and verifies it against **nothing**;
- it writes `~/.grok/bin`, `~/.grok/config.toml`, and shell completions;
- it symlinks into `~/.local/bin` and `/usr/local/bin`;
- it appends `export PATH="$HOME/.grok/bin:$PATH"` to `.bashrc`, `.zshrc` or
  `config.fish`, backing the file up first, and on macOS edits `.bash_profile` too.

That last one is not a cosmetic clash. `riabuild-cli/CLAUDE.md` documents
"`~/.riabuild/bin` leads `PATH` inside the environment shell" as load-bearing for three
separate things — the `claude` launcher, the clipboard shims, and the `xdg-open` that
carries links to the laptop — and a stray `PATH` prepend in a dotfile is exactly what
demotes it. Nothing errors when that happens. The developer's own `claude` simply starts
instead of riabuild's.

So riabuild owns Grok Build the way it owns everything else, and — because the point of
an agent in this environment is that it gets on with the work — every launcher turns tool
approvals off.

## What is new, and what is not

| | Where it lives | Why |
|---|---|---|
| the grok binary | `~/.riabuild/grok/<version>/grok` | riabuild owns every tool it installs |
| the mirrored artifact | a `Clubria/riabuild` release | upstream publishes no digest — see §1 |
| its sha256 | the `GROK_BUILDS` table in `tools.rs` | verified against a digest riabuild published |
| the nine profiles | `~/.riabuild/grok/1` … `/9` | `GROK_HOME` is what separates sign-ins — see §2 |
| the ten launchers | `~/.riabuild/bin/grok`, `grok-1` … `grok-9` | the only thing that names a `GROK_HOME` |
| bypassed approvals | `--permission-mode bypassPermissions`, on the launcher | CLI beats config, and config is the developer's file — see §3 |
| an xAI sign-in | the developer's own account | riabuild brokers nothing here |

Nothing about the org, the dashboard, or the secret-brokering path changes. riabuild-web
gains no field, serves no Grok setting, and is not consulted by any of this.

## 1. Distribution: riabuild republishes Grok Build

Grok Build is the **second** tool riabuild owns that cannot satisfy the rule in
`../../../CLAUDE.md` as written, and it fails a different half of it than ngrok does.

ngrok's problem is the *pin*: Equinox serves one floating build per platform and the
version in the URL is decorative, so `ngrok-v9.99.9-…` returns the same bytes as
`ngrok-v3-stable-…`. xAI has no such problem. Its URLs are honest:

```
https://x.ai/cli/grok-1.0.5-linux-x86_64      200, 166,854,368 bytes
https://x.ai/cli/grok-9.99.9-linux-x86_64     404
```

What xAI does not publish is a **digest**. There is no checksum file at any spelling
beside the artifact — `.sha256`, `.sha256sum`, `checksums.txt` and friends all 404 — and
`install.sh` verifies nothing at all. Its entire integrity check is:

```sh
chmod +x "$binary_tmp"
if ! "$binary_tmp" --version </dev/null >/dev/null 2>&1; then
    echo "Error: downloaded grok failed to run; keeping the existing install." >&2
```

"It runs" is not "it is the right bytes". So riabuild takes a copy of the bytes a
maintainer verified and points at that, exactly as it does for ngrok:
`packaging/grok/mirror.sh` downloads the four builds, reads the version back out of the
one it can execute, prints each digest, and uploads them under a `grok-v<version>` tag.

### Why mirror at all, when the URL is honest?

The alternative was `Checksum::Pinned` against xAI's own URL, and it is worse in the one
way that matters. A version re-cut under the same name — a rebuild, a re-tag, a CDN that
starts serving something else — becomes a checksum mismatch, and `tools::install` refuses
to install rather than running unverified bytes. That is the correct behaviour and it is
also a **hard failure on every laptop at once**, for bytes nobody can fetch any more. A
mirror riabuild holds keeps working, and can be re-verified against upstream at leisure.

The cost is real and worth writing down: 134–167 MB per platform, about **588 MB** per
mirrored version. GitHub's per-file limit is 2 GB so this is a housekeeping question
rather than a hard one, but mirror tags stay rare — one per version bump, which is a code
change anyway — and old `grok-v*` tags should be pruned once no released riabuild pins
them.

### The bytes are not repacked

xAI serves an **uncompressed executable**: no tarball, no zip, nothing beside it. That is
new for this codebase, and it is why `archive::Kind` grew a third variant.

The tempting alternative was to have `mirror.sh` wrap the binary in a `.tar.gz` so nothing
in the Rust had to change. That is precisely wrong. The digest in `tools.rs` would then
describe *riabuild's repack* rather than the bytes xAI served, putting an unverifiable
transformation between what a maintainer checked and what a laptop runs — the opposite of
what pinning is for. `riabuild-cli/CLAUDE.md` already says this about ngrok, whose assets
keep the container upstream published them in so "nothing has to trust a repacking step".

So `Kind::Raw` reads the download straight through, and the mirrored asset is renamed to
`.bin` and nothing else. Renaming a file does not change its contents, so each pinned
digest is still the digest of the upstream artifact — and `Kind::of` keeps deriving the
container from the asset name, which is what makes the name and the container unable to
drift apart.

`Raw` is spelled as an explicit `.bin` suffix rather than inferred from "an extension I do
not recognise". Inferring it would make every future typo, and every asset in a container
riabuild has not learned yet — a `.pkg`, a `.deb`, an `.xz` — install as though it were an
executable, and fail when the developer runs it rather than here.

### Two smaller things that fall out of it being a Rust binary

**The platform words are Rust's, not Go's.** gh, Infisical and ngrok are Go programs
published as `amd64`/`arm64`; xAI names its artifacts after the target triple's halves —
`linux-x86_64`, `macos-aarch64`. Reaching for the existing `go_arch()` helper here builds
a URL that 404s on every machine, and nothing else in the codebase would notice, so
`grok_asset_names_use_rusts_platform_words_and_not_gos` exists to fail loudly.

**Nothing needs to be on `PATH` to start it.** The Linux build is a `static-pie` ELF. That
is the one thing that makes this task genuinely cheaper than `codex_cli` rather than a
copy of it: Codex is a Node script whose `#!/usr/bin/env node` shebang sends the machine
looking for a Node first, so both its probe and its launcher have to carry riabuild's own
Node. Grok Build's launcher carries none, and `depends_on()` is empty rather than naming
`toolchain`.

## 2. Nine accounts, because `GROK_HOME` really does separate sign-ins

The same claim as Codex's nine, resting on the same kind of evidence. Grok Build stores
its credentials in `$GROK_HOME/auth.json`, keyed by auth scope, and in no OS keychain — so
nothing collides the way it would for a tool that keyed one. `GROK_HOME` also carries the
rest of that account's local state: `config.toml`, sessions, MCP registrations, hooks and
plugins.

`~/.riabuild/grok/1` … `/9`, numbered rather than named by uuid, for the reason the Codex
profiles are: the set is fixed, nothing is ever created or renumbered, so `grok-3` and
`~/.riabuild/grok/3` are obviously the same thing to anyone reading their own disk.
Claude Code's uuids exist because *its* accounts can be deleted and renumbered, which
makes position the account number and forces the directory name to survive that.

The nine exist from the first run rather than on demand, again for Codex's reason:
riabuild signs nobody in to Grok Build, so there is no moment at which it would learn that
a developer wants a second one. That is also why there is no `riabuild grok
new|delete|primary`.

One difference from Codex worth recording, because it changes what a check is *for*. Codex
refuses to start against a `CODEX_HOME` that does not exist — `Error finding codex home` —
so creating all nine is repairing a machine that would otherwise be broken. Grok Build
**creates** a `GROK_HOME` that is not there. riabuild creates the nine anyway, so that
"nine accounts" is a state of the machine `check()` can assert rather than a promise that
comes true the first time each launcher happens to be run.

That same behaviour is why the `--version` probe names a `GROK_HOME` instead of leaving it
unset. An unset one does not merely *read* the developer's `~/.grok` — it brings that
directory into existence on a machine where they may never have had one.

## 3. Bypassing permissions, always

The ask is "enable bypass permissions and always keep it on", and Grok Build offers more
than one way to spell it. The launcher passes:

```
grok --permission-mode bypassPermissions
```

### Why that value

`--permission-mode` takes six values — `default`, `acceptEdits`, `auto`, `dontAsk`,
`bypassPermissions`, `plan` — and only one is a full bypass. The trap is `dontAsk`, which
reads like the thing we want and is its opposite: it *silently denies* every tool that is
not pre-approved, producing a session that looks permissive and does nothing.

### Why a flag and not a config file

Grok Build resolves the launch mode as **CLI beats `[ui]` config beats remote**
(`resolve_effective_yolo` in `xai-grok-shell`), and the config route would have been
writing `permission_mode = "always-approve"` into each profile's `config.toml`. Two
reasons not to:

1. The flag is the only spelling that cannot be silently overridden by something already
   on disk. "Always on" has to mean always.
2. `config.toml` is a file the developer owns and edits — Grok Build's own `/settings` UI
   writes to it. riabuild would be rewriting it underneath them on every run. The launcher
   is riabuild's file and says so on its first line.

This is the same shape as the Claude launcher's `--settings` layer and Codex's `--yolo`:
riabuild's policy travels on the command line, and the developer's own files are left
alone.

### Why it is a default and not an imposition

Grok Build rejects `--permission-mode` twice — *"the argument '--permission-mode <MODE>'
cannot be used multiple times"*, in both the spaced and `=` spellings. So a launcher that
appended it unconditionally would turn

```sh
grok --permission-mode plan      # a developer who wants to plan first
grok --permission-mode ask       # a developer who wants the prompts back
```

into a parser error naming a flag they never typed. That is exactly the trap `--yolo` sets
for the Codex launcher, and the answer is the same: the launcher scans its own arguments
and stands aside wherever the developer expressed a policy of their own.

Three deliberate details in that scan:

- **`--always-approve` / `--yolo` is not matched.** It is a separate boolean, Grok Build
  accepts it happily *alongside* `--permission-mode bypassPermissions`, and both mean the
  same thing — so standing aside for it would buy nothing.
- **The flag goes ahead of `"$@"`.** Grok Build accepts `--permission-mode` as a **root**
  option only; after a subcommand it is `unexpected argument '--permission-mode' found`.
  So `grok mcp list` has to become `grok --permission-mode … mcp list`, not the reverse.
- **The scan is textual, so it over-matches.** `grok -p 'what does --permission-mode do?'`
  makes the launcher stand aside. That is the safe direction to be wrong in: the session
  asks for approvals rather than silently granting them.

### What can still turn it off, and that is correct

A managed-policy pin (`yolo_disabled_by_policy`, from `~/.grok/managed_config.toml` or
`/etc/grok/managed_config.toml`) force-disables the bypass regardless of the flag, and
warns when it does. riabuild does not fight that, and should not: an enterprise deployment
pinning approvals on is a decision made above riabuild's head, and quietly defeating it is
not something a provisioner gets to do.

Note also that the bypass is a **client-side approval policy, not a sandbox**. Grok
Build's separate `GROK_SANDBOX` profile defaults to `off`, and riabuild sets neither it
nor `GROK_SANDBOX_AUTO_ALLOW_BASH` — leaving those to the developer and to any policy
their machine carries.

## 4. What riabuild deliberately does not do

- **Sign anyone in.** A Grok sign-in is the developer's own xAI account. `grok-3 login` is
  one command away and lands in that profile's `GROK_HOME` because the launcher put them
  there.
- **Run `x.ai/cli/install.sh`.** See §Why. `nothing_runs_xais_install_script` is the gate.
- **Broker an xAI credential.** No `XAI_API_KEY` is fetched, stored, or written. There is
  no dashboard field for one, and adding one would put a fourth server-held secret in a
  design that already says out loud what the three it has cost.
- **Copy the Claude launcher's workarounds.** `unset SSH_CONNECTION SSH_CLIENT SSH_TTY`,
  the `WAYLAND_DISPLAY` claim, and the `--settings` layer are all read out of the Claude
  Code binary. None is a fact about Grok Build, and asserting them here would be inventing
  an upstream behaviour rather than accommodating one.
- **Export `GROK_HOME` into the environment shell.** For the reason `CLAUDE_CONFIG_DIR`
  and `CODEX_HOME` are not: one exported value would quietly make all nine profiles share
  a directory, and would follow every `grok` the developer started by any route.

## 5. Everything undocumented this rests on

All of it was read out of Grok Build 1.0.5 — the shipped binary and the Apache-2.0 source
at `xai-org/grok-build` — rather than from anything xAI promises. That is why the smoke
tests in `shims::grok` are `#[ignore]`d rather than deleted: they run the *generated
launcher* against a real install, so an upstream change surfaces as a test failure rather
than as broken laptops. Run `cargo test -- --ignored` when the pin moves.

| Behaviour | Consequence if it changes |
|---|---|
| `GROK_HOME` overrides the config directory | the nine profiles collapse into one account |
| credentials live in `$GROK_HOME/auth.json`, no keychain | same |
| `--permission-mode bypassPermissions` is a root option | `grok mcp list` breaks under the launcher |
| it cannot be passed twice | `grok --permission-mode plan` becomes a parser error |
| it is compatible with `--always-approve` | the stand-aside list is missing a spelling |
| CLI beats `[ui] permission_mode` | "always on" stops being always |
| Grok Build creates a missing `GROK_HOME` | the launcher's `mkdir` becomes load-bearing rather than tidy |
| `--version` prints `grok <semver> (<rev>)` | the version probe reads it as unusable |
| the artifact is a bare executable | `Kind::Raw` is the wrong reader |
| xAI publishes no checksum | the mirror could be retired for `Checksum::Published` |

## 6. Rollout order

`tools.rs` names a mirror tag, so the release has to exist before the code that points at
it does:

1. Run `./packaging/grok/mirror.sh` on a machine with `gh` — it uploads `grok-v1.0.5` and
   prints the `GROK_BUILDS` table.
2. Confirm the printed table matches the one committed in `tools.rs`. It should, byte for
   byte; if it does not, xAI re-cut the version and the committed digests are stale.
3. Merge. Until step 1 has happened, `grok_cli`'s `apply()` fails with a 404 on every
   machine, and the e2e suite's Grok block fails with it.
