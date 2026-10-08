"""Prepare or verify the package version before any release is published."""

import os
from pathlib import Path
import re
import subprocess
import tomllib


def stable_version(value):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value):
        raise ValueError(f"Expected a stable version such as 0.18.0, got {value!r}")
    return tuple(map(int, value.split(".")))


def package_versions():
    manifest = tomllib.loads(Path("Cargo.toml").read_text())
    lock = tomllib.loads(Path("Cargo.lock").read_text())
    packages = [p for p in lock["package"] if p["name"] == manifest["package"]["name"] and "source" not in p]
    if len(packages) != 1:
        raise ValueError("Expected exactly one local package in Cargo.lock")
    return manifest["package"]["version"], packages[0]["version"]


def release_version(mode, version, tag):
    if mode not in ("prepare", "check"):
        raise ValueError("RELEASE_MODE must be prepare or check")
    if mode == "check":
        if not tag.startswith("v"):
            raise ValueError("Release tags must start with v")
        version = tag[1:]
    requested = stable_version(version)
    current, locked = package_versions()
    if mode == "prepare":
        if requested <= stable_version(current):
            raise ValueError(f"Release {version} must be newer than {current}")
        tags = subprocess.check_output(["git", "tag", "--list", "v*"], text=True).splitlines()
        for existing in tags:
            try:
                previous = stable_version(existing[1:])
            except ValueError:
                continue
            if requested <= previous:
                raise ValueError(f"Release {version} must be newer than existing tag {existing}")
        replacements = {
            "Cargo.toml": (r'(\[package\]\s*\n(?:(?!\[)[^\n]*\n)*?version\s*=\s*")[^"]+("[^\n]*)', version),
            "Cargo.lock": (r'(\[\[package\]\]\nname = "xmux"\nversion = ")[^"]+("\n)', version),
        }
        updated = {}
        for filename, (pattern, replacement) in replacements.items():
            content, count = re.subn(pattern, lambda m: m[1] + replacement + m[2], Path(filename).read_text())
            if count != 1:
                raise ValueError(f"Expected exactly one package version in {filename}")
            updated[filename] = content
        for filename, content in updated.items():
            Path(filename).write_text(content)
        current, locked = package_versions()
    if (current, locked) != (version, version):
        raise ValueError(f"Release {version} disagrees with Cargo.toml ({current}) or Cargo.lock ({locked})")
    return f"v{version}"


if __name__ == "__main__":
    tag = release_version(os.environ["RELEASE_MODE"], os.environ.get("RELEASE_VERSION", ""), os.environ.get("RELEASE_TAG", ""))
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"tag={tag}\n")
    print(f"Verified package version for {tag}")
