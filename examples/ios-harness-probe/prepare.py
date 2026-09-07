#!/usr/bin/env python3
"""Prepare an isolated XcodeGen project; does not install or authenticate."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
from urllib.parse import urlsplit


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kernel", type=Path, required=True)
    parser.add_argument("--initrd", type=Path, required=True)
    parser.add_argument("--xcframework", type=Path, required=True)
    parser.add_argument("--header", type=Path, required=True)
    parser.add_argument("--configuration", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--team", required=True)
    parser.add_argument("--bundle-id", default="dev.zseven.rish.harnessprobe")
    args = parser.parse_args()
    configuration = json.loads(args.configuration.read_text())
    if configuration.get("network", "disabled") not in ("disabled", "user-nat"):
        parser.error("network must be disabled or user-nat")
    commands = configuration.get("commands")
    downloads = configuration.get("downloads")
    if downloads is not None:
        if commands is not None or not isinstance(downloads, list) or not 1 <= len(downloads) <= 2:
            parser.error("download-only configuration must have 1..2 downloads and no commands")
        names = set()
        for item in downloads:
            if not isinstance(item, dict):
                parser.error("invalid download descriptor")
            url = urlsplit(item.get("url", ""))
            name = item.get("name")
            if name not in ("codex.tgz", "claude.tgz") or name in names:
                parser.error("fixture names must be unique codex.tgz or claude.tgz")
            if (url.scheme != "https" or url.hostname != "registry.npmjs.org" or
                    url.username is not None or url.password is not None):
                parser.error("fixtures must come from the official HTTPS npm registry")
            try:
                digest = base64.b64decode(item.get("sha512", ""), validate=True)
            except (ValueError, TypeError):
                parser.error("invalid SHA-512 digest")
            if len(digest) != 64:
                parser.error("fixture must include the official package SHA-512 digest")
            names.add(name)
    else:
        if not isinstance(commands, list) or not 1 <= len(commands) <= 8:
            parser.error("configuration must contain 1..8 argv arrays")
        if any(not isinstance(c, list) or not c or
               any(not isinstance(a, str) or "\0" in a for a in c) for c in commands):
            parser.error("each command must be a nonempty array of NUL-free strings")
    if configuration.get("memory_mib", 1024) not in (512, 1024, 1536, 2048):
        parser.error("memory_mib must be 512, 1024, 1536, or 2048")
    inputs = {"kernel": args.kernel, "harness.cpio": args.initrd,
              "probe.json": args.configuration, "rish.h": args.header}
    for path in inputs.values():
        if not path.is_file():
            parser.error(f"missing input: {path}")
    if not args.xcframework.is_dir():
        parser.error("missing XCFramework")
    args.output.mkdir(parents=True, exist_ok=True)
    provenance = {}
    for name, path in inputs.items():
        source = path.resolve()
        destination = args.output / name
        if source != destination.resolve():
            shutil.copy2(source, destination)
        hasher = hashlib.sha256()
        with source.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                hasher.update(chunk)
        provenance[name] = {"source": str(source), "sha256": hasher.hexdigest()}
    source = Path(__file__).with_name("HarnessProbe.swift").resolve()
    target = {
        "type": "application", "platform": "iOS",
        "sources": [{"path": str(source)}] + [
            {"path": name, "buildPhase": "resources"}
            for name in ("kernel", "harness.cpio", "probe.json")],
        "settings": {"base": {
            "PRODUCT_BUNDLE_IDENTIFIER": args.bundle_id,
            "DEVELOPMENT_TEAM": args.team,
            "GENERATE_INFOPLIST_FILE": "YES",
            "INFOPLIST_KEY_UILaunchScreen_Generation": "YES",
            "SWIFT_OBJC_BRIDGING_HEADER": "rish.h",
            "SWIFT_VERSION": "5.0", "TARGETED_DEVICE_FAMILY": "1,2"}},
        "dependencies": [{"framework": str(args.xcframework.resolve()), "embed": False}],
    }
    spec = {"name": "RishHarnessProbe", "options": {"deploymentTarget": {"iOS": "18.0"}},
            "targets": {"RishHarnessProbe": target}}
    (args.output / "project.json").write_text(json.dumps(spec, indent=2) + "\n")
    (args.output / "inputs.json").write_text(json.dumps(provenance, indent=2) + "\n")
    subprocess.run(["xcodegen", "generate", "--spec", str(args.output / "project.json"),
                    "--project", str(args.output)], check=True)


if __name__ == "__main__":
    main()
