#!/usr/bin/env python3
"""Exercise apk metadata, offline installs, upgrades and removals in a temporary root.

Uses stand-in binaries; skips hooks because this root has no procd/uci.
The OpenWrt container roundtrip separately exercises lifecycle hooks.
Run with APK_TOOL pointing to apk-tools >= 3 and fakeroot on PATH.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

HERE = Path(__file__).resolve().parents[1]
APK = os.environ.get("APK_TOOL", "apk")


def run(*args, **kwargs):
    return subprocess.check_output([str(arg) for arg in args], text=True, **kwargs)


def metadata(package):
    return json.loads(run(APK, "adbdump", "--format", "json", package))


with tempfile.TemporaryDirectory(prefix="cpxy-apk-test-") as directory:
    work = Path(directory)
    bins = work / "bins"
    bins.mkdir()
    for name in ("cpxy-client", "cpxy-router", "cpxy-dns-split", "cpxy-tsgw"):
        (bins / name).write_text("#!/bin/sh\nexit 0\n")
    packages = []
    for version in ("1.0.0-r9", "1.0.0-r10"):
        out = work / version
        run(HERE / "build-apk.sh", "x86_64", version, bins, out)
        packages.append(out / f"cpxy-{version}.apk")
    data = metadata(packages[0])
    assert data["info"]["name"] == "cpxy"
    assert data["info"]["arch"] == "x86_64"
    assert {"ip-full", "kmod-tun", "!cpxy-router"} <= set(data["info"]["depends"])
    assert {"post-install", "pre-upgrade", "post-upgrade", "pre-deinstall", "post-deinstall"} <= data["scripts"].keys()
    paths = {entry.get("name", ""): entry for entry in data["paths"]}
    for entry in paths.values():
        assert entry["acl"]["user"] == "root", entry
        assert entry["acl"]["group"] == "root", entry
        for file in entry.get("files", []):
            assert file["acl"]["user"] == file["acl"]["group"] == "root", file
    config = next(file for file in paths["etc/config"]["files"] if file["name"] == "cpxy")
    assert config["acl"]["mode"] == 0o600
    assert not any(path.startswith("etc/apk") or path.startswith("etc/opkg") for path in paths)

    # Hook dispatch and upgrade markers without invoking host services.
    hook_root = work / "hooks"
    (hook_root / "lib").mkdir(parents=True)
    (hook_root / "lib/functions.sh").write_text(
        'default_postinst() { printf "install:%s\\n" "${PKG_UPGRADE:-0}" >>"$HOOK_LOG"; }\n'
        'default_prerm() { printf "remove:%s\\n" "${PKG_UPGRADE:-0}" >>"$HOOK_LOG"; }\n'
    )
    hook_log = work / "hooks.log"
    env = dict(os.environ, IPKG_INSTROOT=str(hook_root), HOOK_LOG=str(hook_log))
    for name in ("post-install", "pre-upgrade", "post-upgrade", "pre-deinstall"):
        script = work / name
        script.write_text(data["scripts"][name])
        run("sh", script, env=env)
    assert hook_log.read_text().splitlines() == ["install:0", "remove:1", "install:1", "remove:0"]

    # Native ARM64 metadata and reproducible unsigned builds.
    run(HERE / "build-apk.sh", "aarch64", "1.0.0-r10", bins, work / "arm64")
    assert metadata(work / "arm64/cpxy-1.0.0-r10.apk")["info"]["arch"] == "aarch64"
    run(HERE / "build-apk.sh", "x86_64", "1.0.0-r10", bins, work / "repeat")
    assert (work / "repeat/cpxy-1.0.0-r10.apk").read_bytes() == packages[1].read_bytes()

    # The optional signing path must work with apk's normal trust mechanism.
    keys = work / "keys"
    keys.mkdir()
    key = work / "test.key"
    run("openssl", "genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", key)
    run("openssl", "pkey", "-in", key, "-pubout", "-out", keys / "test.pub")
    run(HERE / "build-apk.sh", "x86_64", "1.0.0-r10", bins, work / "signed", env=dict(os.environ, CPXY_APK_SIGN_KEY=str(key)))
    run(APK, "--keys-dir", keys, "verify", work / "signed/cpxy-1.0.0-r10.apk")

    # Dummy dependencies allow a fully offline package-manager test without
    # installing firmware-specific kernel modules into this isolated root.
    deps = []
    for name in ("ip-full", "kmod-tun"):
        package = work / f"{name}.apk"
        run(APK, "mkpkg", "--info", f"name:{name}", "--info", "version:1-r0", "--info", "arch:x86_64", "--output", package)
        deps.append(package)
    root = work / "root"
    root.mkdir()
    apk = [APK, "--root", root, "--arch", "x86_64", "--no-network", "--allow-untrusted", "--repositories-file", "/dev/null"]
    run(*apk, "add", "--usermode", "--no-scripts", "--initdb", *deps, packages[0])
    assert run(*apk, "info", "-e", "cpxy=1.0.0-r9").strip() == "cpxy"
    installed_config = root / "etc/config/cpxy"
    contents = installed_config.read_text() + "\n# user configuration\n"
    installed_config.write_text(contents)
    run(*apk, "add", "--usermode", "--no-scripts", packages[1])
    assert run(*apk, "info", "-e", "cpxy=1.0.0-r10").strip() == "cpxy"
    assert installed_config.read_text() == contents
    expected_hash = (root / "lib/apk/packages/cpxy.conffiles_static").read_text().split()[1]
    original_hash = hashlib.sha256((HERE / "files/etc/config/cpxy").read_bytes()).hexdigest()
    assert expected_hash == original_hash
    assert "/etc/init.d/cpxy-router" in (root / "lib/apk/packages/cpxy.list").read_text()
    run(*apk, "del", "--no-scripts", "cpxy")
    assert not (root / "usr/bin/cpxy-router").exists()
    assert not (root / "etc/init.d/cpxy-router").exists()
    assert not (root / "lib/apk/packages/cpxy.list").exists()
    assert installed_config.read_text() == contents
print("apk metadata, hooks, signing, reproducibility, offline install, upgrade, config preservation and removal: passed")
