#!/usr/bin/env python3
"""Writes module.modulemap, umbrella.h and clang-args.txt for the libraries in
sdk.json (Linux headers have no module maps; modules decide what gets dlopen()ed).

Usage: gen-sdk.py [--config sdk.json] [--sysroot DIR] --out DIR
"""

import argparse
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


class PkgConfig:
    def __init__(self, sysroot):
        self.sysroot = sysroot
        self.env = dict(os.environ)
        if sysroot:
            host = subprocess.run(
                ["pkg-config", "--variable", "pc_path", "pkg-config"], capture_output=True, text=True
            ).stdout.strip().split(":")
            local = [d for d in host if d]
            self.env["PKG_CONFIG_SYSROOT_DIR"] = sysroot
            self.env["PKG_CONFIG_LIBDIR"] = ":".join([sysroot + d for d in local] + local)
        self.cache = {}

    def run(self, *args):
        key = args
        if key not in self.cache:
            p = subprocess.run(["pkg-config", *args], capture_output=True, text=True, env=self.env)
            if p.returncode != 0:
                raise RuntimeError(f"pkg-config {' '.join(args)}: {p.stderr.strip()}")
            self.cache[key] = p.stdout.split()
        return self.cache[key]

    def exists(self, pkg):
        return subprocess.run(["pkg-config", "--exists", pkg], env=self.env).returncode == 0

    def host_path(self, path):
        """Paths from pkg-config are prefixed with the sysroot, fall back to the
        host when the sysroot doesn't have them (e.g. libc headers)."""
        if self.sysroot and path.startswith(self.sysroot) and not os.path.exists(path):
            return path[len(self.sysroot):]
        return path

    def include_dirs(self, pkg):
        return [self.host_path(f[2:]) for f in self.run("--cflags-only-I", pkg)]

    def defines(self, pkg):
        return [f for f in self.run("--cflags-only-other", pkg) if f.startswith("-D")]

    def libs(self, pkg):
        return [f[2:] for f in self.run("--libs-only-l", pkg)]

    def requires(self, pkg):
        names = []
        for flag in ("--print-requires", "--print-requires-private"):
            p = subprocess.run(["pkg-config", flag, pkg], capture_output=True, text=True, env=self.env)
            for line in p.stdout.splitlines():
                if line.strip():
                    names.append(line.split()[0])
        return names


def soname_cache():
    """lib name -> soname from the dynamic linker cache (`ldconfig -p`)."""
    arch = {"x86_64": "x86-64", "aarch64": "AArch64"}.get(os.uname().machine, "")
    out = subprocess.run(["/sbin/ldconfig", "-p"], capture_output=True, text=True).stdout
    entries = []
    for line in out.splitlines()[1:]:
        m = re.match(r"\s*(\S+) \(([^)]*)\) => (\S+)", line)
        if m and (not arch or arch in m.group(2)):
            entries.append(m.group(1))
    return entries


def soname(name, cache):
    pattern = re.compile(rf"^lib{re.escape(name)}\.so\.\d+$")
    matches = sorted((e for e in cache if pattern.match(e)), key=len)
    return matches[0] if matches else None


def module_name(pkg):
    name = re.sub(r"-?[\d.]+$", "", pkg)
    return re.sub(r"[^A-Za-z0-9_]", "_", name)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", default=os.path.join(HERE, "sdk.json"))
    ap.add_argument("--sysroot", default=os.environ.get("NS_SYSROOT"))
    ap.add_argument("--out", required=True)
    ap.add_argument("--only", help="comma separated module names to include")
    args = ap.parse_args()

    sysroot = os.path.abspath(args.sysroot) if args.sysroot else None
    pc = PkgConfig(sysroot)
    config = json.load(open(args.config))
    modules = config["modules"]
    if args.only:
        wanted = set(args.only.split(","))
        modules = [m for m in modules if m["name"] in wanted]

    # drop modules whose packages aren't available
    usable = []
    for m in modules:
        missing = [p for p in m.get("pkg-config", []) if not pc.exists(p)]
        if missing:
            print(f"warning: skipping module {m['name']}, missing pkg-config packages: {', '.join(missing)}", file=sys.stderr)
            continue
        usable.append(m)
    modules = [dict(m) for m in usable]

    def own_dirs(pkg):
        deps = set()
        for r in pc.requires(pkg):
            if pc.exists(r):
                deps.update(pc.include_dirs(r))
        return [d for d in pc.include_dirs(pkg) if d not in deps]

    # transitive dependencies not covered by a configured module get their own module
    claimed = {p for m in modules for p in m.get("pkg-config", [])}
    queue = [p for m in modules for p in m.get("pkg-config", [])]
    seen = set(queue)
    auto = []
    while queue:
        pkg = queue.pop(0)
        for r in pc.requires(pkg):
            if r in seen or not pc.exists(r):
                continue
            seen.add(r)
            queue.append(r)
            if r not in claimed:
                auto.append({"name": module_name(r), "pkg-config": [r], "auto": True})
    modules += auto

    owner = {}  # include dir -> module name
    sonames = soname_cache()
    for m in modules:
        # explicit directories: the host copy wins (it's found first by clang),
        # the sysroot is only searched after the system directories
        dirs = [d if os.path.exists(d) or not sysroot else sysroot + d for d in m.get("umbrella", [])]
        for pkg in m.get("pkg-config", []):
            dirs += own_dirs(pkg)
        m["dirs"] = []
        for d in dirs:
            if d not in owner and os.path.isdir(d):
                owner[d] = m["name"]
                m["dirs"].append(d)
        links = list(m.get("link", []))
        for pkg in m.get("pkg-config", []):
            links += pc.libs(pkg)
        resolved = []
        for l in dict.fromkeys(links):
            so = soname(l, sonames)
            if so:
                resolved.append(so)
            else:
                print(f"warning: {m['name']}: no runtime library for -l{l}, skipped", file=sys.stderr)
        m["links"] = resolved

    # packages whose headers live in another package's directory (cairo-gobject,
    # pangocairo...) contribute their libraries to that module
    by_name = {m["name"]: m for m in modules}
    for m in list(modules):
        if m["dirs"] or not m.get("auto"):
            continue
        for d in pc.include_dirs(m["pkg-config"][0]):
            target = by_name.get(owner.get(d))
            if target and target is not m:
                target["links"] = list(dict.fromkeys(target["links"] + m["links"]))
                break
    modules = [m for m in modules if m["dirs"] or m.get("files")]

    os.makedirs(args.out, exist_ok=True)
    with open(os.path.join(args.out, "module.modulemap"), "w") as f:
        f.write("// Generated by metadata-generator/linux/gen-sdk.py, do not edit.\n")
        for m in modules:
            f.write(f"module {m['name']} [system] {{\n")
            for i, d in enumerate(m["dirs"]):
                f.write(f'  module dir{i} {{ umbrella "{d}" }}\n')
            for i, h in enumerate(m.get("files", [])):
                f.write(f'  module file{i} {{ header "{pc.host_path(h)}" }}\n')
            for l in m["links"]:
                f.write(f'  link "{l}"\n')
            f.write("}\n\n")

    with open(os.path.join(args.out, "umbrella.h"), "w") as f:
        for m in modules:
            if m.get("headers"):
                f.write(f"// {m['name']}\n")
                for h in m["headers"]:
                    f.write(f"#include <{h}>\n")

    flags = []
    for m in modules:
        for pkg in m.get("pkg-config", []):
            flags += [f"-I{d}" for d in pc.include_dirs(pkg)]
            flags += pc.defines(pkg)
    if sysroot:
        flags.append(f"-idirafter{sysroot}/usr/include")
    flags = list(dict.fromkeys(flags))
    with open(os.path.join(args.out, "clang-args.txt"), "w") as f:
        f.write("\n".join(flags) + "\n")

    for m in modules:
        print(f"{m['name']:<14} {len(m['dirs'])} dir(s)  links: {', '.join(m['links'])}")


if __name__ == "__main__":
    main()
