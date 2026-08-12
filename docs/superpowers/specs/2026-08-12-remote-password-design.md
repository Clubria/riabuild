# A password is a way in, not a failure

`riabuild remote` treats one outcome as fatal that is not:

```
riabuild stopped: authorising riabuild's key on ssh.cloudcli.ai
    the key was copied, but signing in with it still does not work
    do this: Add this line to ~/.ssh/authorized_keys on ssh.cloudcli.ai, …
```

`ssh-copy-id` exited 0, so the key reached the server. The follow-up probe —
`ssh -o BatchMode=yes`, which by construction can never prompt — still says no.
That happens for ordinary reasons: `AuthorizedKeysFile` points somewhere else,
the account's home is on a mode the server refuses to trust, `sshd` is
configured `AuthenticationMethods publickey,password`. None of them mean the
developer cannot get in. They mean the developer gets in *with their password*,
which is how they got in before riabuild existed.

So riabuild stops a developer who has a working way onto the machine, and does
it at the point where all the remaining work — installing the server's riabuild,
minting its session, lending it a GitHub sign-in — would have succeeded.

This changes two things, and the second is the larger one.

## Part 1 — warn, do not stop

`authorise` already establishes, before it does anything, whether the server
offers `password` or `keyboard-interactive`. Everything past that guard runs
knowing a way in exists. So:

| Outcome | Today | After |
|---|---|---|
| Server offers keys only | stop | **stop** — no password exists to fall back to |
| riabuild's public key is missing | stop | **stop** — nothing to paste, nothing to install |
| Host key does not match the pin | stop | **stop** — this is not the server riabuild trusts |
| `ssh-copy-id` not installed | stop | warn, carry on |
| `ssh-copy-id` exits non-zero | stop | warn, carry on |
| Key copied, sign-in still refused | stop | warn, carry on |

The rule underneath: **riabuild stops when there is no way in, not when the
convenient way in failed.** The warning keeps the paste-this-line remedy, because
that is still how the developer stops being asked for a password on every future
run; it just stops being an ultimatum.

`authorise` returns `Ok(())` on the warning paths. Nothing downstream needs to
know: every subsequent `ssh` carries `IdentitiesOnly=yes`, which restricts which
*keys* are offered and not which *methods*, so `ssh` falls back to password
authentication on its own.

## Part 2 — the password riabuild asks for once

Falling back to a password is only tolerable once. `riabuild remote` opens
something like ten separate SSH connections — `resolve_home`, four in the install
step, the session write, `gh-sweep`, `seed-github`, the setup run, the clipboard
forward, the shell — and each is its own process with its own authentication.
Ten password prompts is worse than the failure this replaces.

riabuild will therefore ask once and remember.

### Why `SSH_ASKPASS`, and nothing else

`authorise.rs`'s module doc argues at length that riabuild must never hold a
password, on two grounds. Both were true when written and one still is:

- **`Ui::ask` echoes.** True. It is a plain `read_line`. This is why a new
  no-echo read is part of the work rather than a reuse of an existing prompt.
- **There is no channel from riabuild to `ssh` that avoids `ps`.** This was the
  load-bearing claim, and it is wrong. `ssh` will not read a password from stdin,
  but it will run a program named by `SSH_ASKPASS` and read the answer from that
  program's **stdout pipe**. No argument vector, no environment variable, nothing
  `ps` can show. That is the supported mechanism, it is what every GUI SSH
  frontend uses, and it is not riabuild reimplementing SSH's password protocol.

The alternative — `sshpass -p <password>` — is exactly what the doc rules out:
the secret sits in the argument vector, visible to every other developer on a
shared box, and `sshpass` is not a tool riabuild owns and verifies.

So the doc's conclusion changes while its reasoning survives: riabuild still
never reads a password into a `String` it hands to a command line. It reads one
into a `String` it writes to a pipe `ssh` is holding open.

### The helper

`SSH_ASKPASS` names a bare executable path — `ssh` appends the prompt text as
`argv[1]`, leaving no room for a subcommand — so riabuild cannot point it at its
own binary. It writes a two-line shim, the same shape `shims::exec_shim` already
generates for `gh` and `pnpm`:

```sh
#!/bin/sh
exec '/opt/homebrew/bin/riabuild' internal askpass "$@"
```

at `<root>/ssh/askpass`, mode 0700, rewritten on every run so a moved binary
never leaves a shim pointing at nothing.

Every `ssh`, `mosh` and `ssh-copy-id` riabuild starts for a server then carries
three environment entries:

| Variable | Value |
|---|---|
| `SSH_ASKPASS` | the shim's path |
| `SSH_ASKPASS_REQUIRE` | `force` |
| `RIABUILD_ASKPASS_ACCOUNT` | `remote-password:<remote hash>` |

Only the *account name* travels in the environment. `SSH_ASKPASS_REQUIRE=force`
is what makes `ssh` consult the helper even when a terminal exists; it needs
OpenSSH 8.4 or newer, and older clients ignore the variable and prompt on the
terminal themselves — degraded to today's behaviour, never broken.

`riabuild internal askpass` then:

1. reads `RIABUILD_ASKPASS_ACCOUNT`, and fails if it is unset — an askpass
   invoked by something other than riabuild has no business answering;
2. if the prompt text names a *passphrase* rather than a password, asks the
   developer and **does not save it**. That is the passphrase to their own key,
   which riabuild neither owns nor manages;
3. otherwise looks the account up. A hit is printed to stdout and nothing is
   asked. A miss is asked for once, with echo off, on `/dev/tty` — never stdout,
   which is the answer channel — then saved, then printed.

A stored password that the server rejects is cleared, so the next run asks again
rather than looping on a stale one. `riabuild remote forget` deletes it beside
the session token it already revokes.

### Where it is stored, and the invariant that moves

The session token goes in the OS keychain, and so does this: `security` on macOS,
`secret-tool` on Linux, under `remote-password:<hash>`.

Machines without a keyring — a Linux box with no `secret-tool`, a CI runner, the
e2e container — fall back to a 0600 file under `~/.riabuild/ssh/`, through the
`FileKeychain` that already exists for servers. This is a deliberate widening of
**"No secrets in `~/.riabuild/`"**, and `CLAUDE.md` is amended in the same change
rather than left quietly false.

The reasoning for that invariant is that a secret on disk outlives the machine it
was meant for: it ends up in backups, in synced folders, in tarballs sent to
support. That is still true, and it is why the keychain is preferred wherever one
exists. What the invariant was written to protect is the Infisical org
credential, which is still brokered per use and still never written down. An SSH
password for one account on one server, at 0600, in a directory riabuild creates
at 0700, is a smaller thing — and the alternative on a keyring-less machine is
not "no password on disk", it is "riabuild is unusable there".

The platform decision — is there a keyring on this machine? — lives in
`keychain/`, which `CLAUDE.md` already names as one of the few files permitted to
know which OS it is running on.

## What this does not do

- No SSH connection multiplexing. `ControlMaster` would collapse the ten
  connections into one, but a saved password already removes the ten prompts,
  which is the problem worth solving. A control socket, its teardown, and its
  stale-socket handling are a separate change if one is ever wanted.
- No change to `--check`. It calls `can_sign_in` instead of `authorise` and never
  writes to the server; a run that cannot sign in is still a check *result*.
- No change to host-key trust. A pin that does not match is still fatal, and
  still says so in the words `host_key` owns.

## Testing

Unit, against `FakeRunner` and a temporary root:

- Each of the three downgraded outcomes returns `Ok` and warns; each of the three
  fatal ones still returns the `Failure` naming its own cause.
- The warning still carries the public key and `authorized_keys`, so the remedy
  survives the change in severity.
- Every `ssh` invocation for a remote carries all three environment entries — the
  tunnel included, which reaches them through a `Tunnel` field rather than by
  learning what a `Remote` is.
- The helper answers from the store without prompting; a passphrase prompt is
  never saved; a missing `RIABUILD_ASKPASS_ACCOUNT` fails.
- `remote forget` removes the stored password.
- `for_account_or_file` picks the file only where there is no keyring.

The end-to-end suite is untouched: its container authenticates by key through an
agent, which is the path that already worked and still does.
