#!/usr/bin/env python3
"""Package target/release into dist/knobdeck-sd-plugin-adapter-<version>-<platform>.zip (+ .sha256).
usage: package.py <platform>   e.g. linux-x86_64, macos-aarch64, macos-x86_64, windows-x86_64"""
import hashlib, json, os, re, sys, zipfile

platform = sys.argv[1]
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
version = re.search(r'^version\s*=\s*"([^"]+)"', open(f"{root}/bridge/Cargo.toml").read(), re.M).group(1)
exe = ".exe" if platform.startswith("windows") else ""
manifest = json.load(open(f"{root}/adapter.json"))
manifest["version"] = version
manifest["run"]["executable"] = f"knobdeck-sd-plugin-adapter{exe}"
os.makedirs(f"{root}/dist", exist_ok=True)
name = f"knobdeck-sd-plugin-adapter-{version}-{platform}.zip"
out = f"{root}/dist/{name}"
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("adapter.json", json.dumps(manifest, indent=2))
    for f in (f"knobdeck-sd-plugin-adapter{exe}", f"bridge-shim{exe}"):
        info = zipfile.ZipInfo(f)
        info.external_attr = 0o755 << 16  # executable
        info.compress_type = zipfile.ZIP_DEFLATED
        z.writestr(info, open(f"{root}/target/release/{f}", "rb").read())
    z.write(f"{root}/LICENSE", "LICENSE")
digest = hashlib.sha256(open(out, "rb").read()).hexdigest()
open(out + ".sha256", "w").write(f"{digest}  {name}\n")
print(name, digest)
