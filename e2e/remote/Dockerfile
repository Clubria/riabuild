# A stand-in for "a Linux box a developer just got SSH access to" — the
# shape `riabuild remote` provisions against. One unprivileged account,
# shared the way a real build box would be between two developers, with its
# `authorized_keys` pre-seeded so `run.sh` can authenticate before riabuild
# has installed anything of its own.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server ca-certificates \
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
