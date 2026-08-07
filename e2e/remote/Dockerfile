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
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
