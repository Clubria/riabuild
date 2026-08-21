# A stand-in for "a Linux box a developer just got SSH access to" — the
# shape `riabuild remote` provisions against. One unprivileged account,
# shared the way a real build box would be between two developers, with its
# `authorized_keys` pre-seeded so `run.sh` can authenticate before riabuild
# has installed anything of its own.
FROM debian:bookworm-slim
# Three packages, and each one is argued for here because the temptation on
# every failed run is to add a fourth and see whether it helps.
#
#   openssh-server   the whole point of the image: `riabuild remote` arrives
#                    over a real SSH connection.
#   ca-certificates  riabuild fetches its own release, Node, pnpm, `gh`,
#                    `infisical` and `ngrok` over HTTPS from in here.
#   git              the one tool riabuild expects a machine to already have,
#                    and the only one. `git_credentials` runs `gh auth
#                    setup-git`, and `project`, `repo_status` and `env_local`
#                    each shell out to `git` after it. Without it the run stops
#                    at `git_credentials` with gh's own "unable to find git
#                    executable in PATH" — riabuild correctly reporting a gap in
#                    the *machine*, which `known_gap` must never forgive, and
#                    which is therefore this file's to close rather than the
#                    gate's.
#
# Nothing else is coming, and that is a property of riabuild rather than of
# this image being lucky. "riabuild owns every tool it installs" means it
# downloads them itself, and it unpacks them in-process — `tar`, `flate2` and
# `zip` are linked into the binary — so there is no `tar`, `xz-utils`, `curl`,
# `unzip` or `wget` here to be missing. `sha256sum`, the one other program the
# server side runs (`install::binary`, verifying what it copied), is coreutils
# and so is already present. A future missing-tool failure on this container
# should be read as riabuild having grown a dependency on the host, not as this
# list being too short.
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server ca-certificates git \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -m -s /bin/sh shared \
 && mkdir -p /run/sshd /home/shared/.ssh \
 && chown -R shared:shared /home/shared/.ssh \
 && chmod 700 /home/shared/.ssh
# One account, several developers — the shape this test exists to exercise.
COPY --chown=shared:shared authorized_keys /home/shared/.ssh/authorized_keys
RUN chmod 600 /home/shared/.ssh/authorized_keys
# `PermitUserEnvironment` is how `run.sh` points the server's own riabuild at
# its stand-in riabuild-web, and it is here rather than in the script because
# it is sshd configuration and sshd is baked in.
#
# The variable cannot come from riabuild: the server runs as
# `env 'K=V' … '/abs/riabuild'` and `remote::env_prefix` chooses those four
# variables, none of which is `RIABUILD_API_URL`. Nothing in riabuild should
# grow a flag for it either — a laptop that could redirect a server's dashboard
# is a laptop that can point somebody else's box at a dashboard of its
# choosing. So the *server* declares where its dashboard is, which is a
# statement a server is entitled to make about itself, and this container is
# entirely the harness's.
#
# Off everywhere else for good reason — it lets anyone who can write
# `~/.ssh/environment` set `LD_PRELOAD` for every later login — and that is
# exactly why it is written here, in the file that says what this container is
# for, rather than switched on in passing by a shell command.
RUN printf 'PermitUserEnvironment yes\n' >> /etc/ssh/sshd_config
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
