# The Linux client: runs the xmux built from the checkout and the scenario driver.
# The docker CLI lets the driver start and stop the host containers it tests against.
FROM debian:12
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      openssh-client python3 python3-pyte locales procps curl ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && sed -i 's/# en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen && locale-gen \
 && curl -fsSL https://download.docker.com/linux/static/stable/x86_64/docker-28.5.1.tgz \
    | tar xz -C /usr/local/bin --strip-components=1 docker/docker
ENV LANG=en_US.UTF-8
