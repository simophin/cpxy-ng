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

    # The release artifact layout keeps each package label separate while using
    # exactly the same generic binary payload. No additional compilation occurs.
    expected_arches = {
        "aarch64-unknown-linux-musl": {"aarch64", "aarch64_generic", "aarch64_cortex-a53", "aarch64_cortex-a72", "aarch64_cortex-a76"},
        "x86_64-unknown-linux-musl": {"x86_64"},
    }
    variants = {}
    for target, arches in expected_arches.items():
        out = work / target / "apk"
        run(HERE / "build-apk-variants.sh", target, "1.0.0-r10", bins, out)
        assert {path.name for path in out.iterdir()} == arches
        for arch in arches:
            package = out / arch / "cpxy-1.0.0-r10.apk"
            variant = metadata(package)
            assert variant["info"]["arch"] == arch
            assert variant["paths"] == data["paths"], arch
            assert variant["scripts"] == data["scripts"], arch
            variants[arch] = package
    for arch in ("arm_cortex-a7", "aarch64_cortex-a99", "aarch64 aarch64_generic", "../x86_64", ""):
        result = subprocess.run([str(HERE / "build-apk.sh"), arch, "1.0.0-r10", str(bins), str(work / "invalid")], capture_output=True)
        assert result.returncode == 2, (arch, result.stderr)
        assert b"unsupported apk architecture" in result.stderr
    result = subprocess.run([str(HERE / "build-apk-variants.sh"), "arm-unknown-linux-musleabi", "1.0.0-r10", str(bins), str(work / "invalid")], capture_output=True)
    assert result.returncode == 2

    # An OpenWrt arch file takes precedence over apk's compiled default. Generic
    # aarch64 must fail on a Cortex-A53 root; the labelled variant must install.
    armroot = work / "armroot"
    (armroot / "etc/apk").mkdir(parents=True)
    (armroot / "etc/apk/arch").write_text("aarch64_cortex-a53\n")
    armdeps = []
    for name in ("ip-full", "kmod-tun"):
        package = work / f"arm-{name}.apk"
        run(APK, "mkpkg", "--info", f"name:{name}", "--info", "version:1-r0", "--info", "arch:aarch64_cortex-a53", "--output", package)
        armdeps.append(package)
    armapk = [APK, "--root", armroot, "--no-network", "--allow-untrusted", "--repositories-file", "/dev/null"]
    usermode = ["--usermode"] if os.geteuid() != 0 else []
    result = subprocess.run([str(arg) for arg in [*armapk, "add", *usermode, "--no-scripts", "--initdb", *armdeps, variants["aarch64"]]], capture_output=True)
    assert result.returncode != 0
    assert b"uninstallable" in result.stderr, result.stderr
    run(*armapk, "add", *usermode, "--no-scripts", "--initdb", *armdeps, variants["aarch64_cortex-a53"])
    assert run(*armapk, "info", "-e", "cpxy=1.0.0-r10").strip() == "cpxy"

    # Reproducible unsigned builds.
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
    # apk refuses --usermode as root (CI containers), but needs it otherwise.
    usermode = ["--usermode"] if os.geteuid() != 0 else []
    run(*apk, "add", *usermode, "--no-scripts", "--initdb", *deps, packages[0])
    assert run(*apk, "info", "-e", "cpxy=1.0.0-r9").strip() == "cpxy"
    installed_config = root / "etc/config/cpxy"
    contents = installed_config.read_text() + "\n# user configuration\n"
    installed_config.write_text(contents)
    run(*apk, "add", *usermode, "--no-scripts", packages[1])
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
print("apk architecture variants, arch-file selection, metadata, hooks, signing, reproducibility, offline install, upgrade, config preservation and removal: passed")
