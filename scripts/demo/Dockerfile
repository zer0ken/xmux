# One image serves every demo machine: the laptop that runs xmux and the servers it
# reaches over ssh. Every machine, user, and session in the recordings comes from here.
# XMUX_VERSION names a published release, which is downloaded and verified, unless
# bin/ holds an xmux built from the checkout. The demo user's password is for the
# server that takes only a password (sessions.sh); every machine sits on a private
# Docker network.
FROM debian:12

ARG XMUX_VERSION
COPY bin/ /tmp/bin/
RUN test -n "$XMUX_VERSION" \
 && apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      openssh-server openssh-client tmux ncurses-term locales bc procps \
      python3 python3-pyte python3-pil curl ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && sed -i 's/# en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen && locale-gen \
 && cd /tmp \
 && if [ -f bin/xmux ]; then install -m 755 bin/xmux /usr/local/bin/xmux; else \
      base="https://github.com/zer0ken/xmux/releases/download/v${XMUX_VERSION}" \
   && tarball="xmux-v${XMUX_VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
   && curl -fsSLO "$base/$tarball" && curl -fsSLO "$base/SHA256SUMS" \
   && grep " \*\?$tarball\$" SHA256SUMS | sha256sum -c - \
   && tar xzf "$tarball" xmux && install -m 755 xmux /usr/local/bin/xmux; fi \
 && rm -rf /tmp/* \
 && useradd -m -s /bin/bash dev && echo dev:demo-pass | chpasswd \
 && mkdir -p /run/sshd /home/dev/.ssh /home/dev/.config/xmux \
 && ssh-keygen -q -t ed25519 -N '' -C dev@laptop -f /home/dev/.ssh/id_ed25519 \
 && cp /home/dev/.ssh/id_ed25519.pub /home/dev/.ssh/authorized_keys \
 && touch /home/dev/.hushlogin

ENV LANG=en_US.UTF-8
COPY ssh_config /home/dev/.ssh/config
COPY xmux.toml /home/dev/.config/xmux/config.toml
COPY sessions.sh record.py encode.py test_record.py /opt/demo/
RUN cd /opt/demo && python3 -m unittest test_record
RUN echo 'PS1='"'"'\[\e[1;32m\]\u@\h\[\e[0m\]:\[\e[1;34m\]\w\[\e[0m\]\$ '"'" >> /home/dev/.bashrc \
 && chown -R dev:dev /home/dev && chmod 700 /home/dev/.ssh \
 && chmod 600 /home/dev/.ssh/id_ed25519 /home/dev/.ssh/authorized_keys /home/dev/.ssh/config
CMD ["/usr/sbin/sshd", "-D", "-e"]
