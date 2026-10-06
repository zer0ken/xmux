# Working Notes: /scripts/e2e/client

## Purpose

The Linux client image: Python with pyte for the driver, the ssh client, and the docker
CLI the suite uses to stop and start hosts. xmux itself is mounted in at run time.

## Module Seams

- The image holds no xmux and no test code; `run.sh` mounts both.

## Invariants

- The image runs any static Linux xmux, so a build from any glibc works.
