"""Writes THIRD_PARTY_NOTICES.txt from what the Windows app actually ships.

Run after changing dependencies: python scripts/generate_notices.py
Packages that include no licence file are listed at the end so they can be checked.
"""
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parent.parent
SELF = "kantan-audio-manager"
TARGET = "x86_64-pc-windows-msvc"
LICENCE_FILE = re.compile(r"^(licen[cs]e|copying|notice|unlicense)", re.IGNORECASE)
# Code bundled inside a package, with its licence in a subfolder.
BUNDLED = {
    "mp3lame-sys": ["lame-4.0/COPYING", "lame-4.0/LICENSE"],
    "aotuv_lancer_vorbis_sys": ["ogg_vendor/COPYING", "vorbis_vendor/COPYING"],
    "ogg_next_sys": ["ogg_vendor/COPYING"],
    "epaint_default_fonts": ["fonts/emoji-icon-font-mit-license.txt", "fonts/OFL.txt", "fonts/UFL.txt"],
}


def run(*args):
    return subprocess.run(args, cwd=ROOT, check=True, capture_output=True, encoding="utf-8").stdout


tree = run("cargo", "tree", "-e", "normal", "--target", TARGET, "--prefix", "none", "-f", "{p}")
shipped = {tuple(line.split()[:2]) for line in tree.splitlines() if line.strip()}
metadata = json.loads(run("cargo", "metadata", "--format-version", "1"))

packages = [
    p
    for p in metadata["packages"]
    if (p["name"], "v" + p["version"]) in shipped and p["name"] != SELF
]
packages.sort(key=lambda p: (p["name"].lower(), p["version"]))

out = [
    "Third-party notices for Kantan Audio Manager",
    "",
    f"Kantan Audio Manager includes the {len(packages)} open-source packages below.",
    "Each entry gives the licence its authors declare, followed by its licence files.",
    "",
]
missing = []
for package in packages:
    folder = pathlib.Path(package["manifest_path"]).parent
    files = sorted(f for f in folder.iterdir() if f.is_file() and LICENCE_FILE.match(f.name))
    files += [folder / extra for extra in BUNDLED.get(package["name"], []) if (folder / extra).is_file()]
    extra_texts = []
    # Packages that don't include their licence: texts fetched from their source repositories.
    fetched = ROOT / "third_party_licenses" / package["name"]
    if fetched.is_dir() and not any(f.parent == folder for f in files):
        for f in sorted(fetched.iterdir()):
            out_name = f.relative_to(fetched).as_posix()
            extra_texts.append((out_name, f.read_text(encoding="utf-8", errors="replace").strip()))
    out += ["=" * 78, f"{package['name']} {package['version']}", f"Licence: {package.get('license') or package.get('license_file') or 'not declared'}"]
    if package.get("repository"):
        out.append(f"Source: {package['repository']}")
    if package.get("authors"):
        out.append("Authors: " + ", ".join(package["authors"]))
    out.append("")
    for file in files:
        out += [f"--- {file.relative_to(folder).as_posix()} ---", file.read_text(encoding="utf-8", errors="replace").strip(), ""]
    for name, body in extra_texts:
        out += [f"--- {name} ---", body, ""]
    if not files and not extra_texts:
        missing.append(package["name"])
        out += ["(This package includes no licence file. See its declared licence and source above.)", ""]

(ROOT / "THIRD_PARTY_NOTICES.txt").write_text("\n".join(out) + "\n", encoding="utf-8", newline="\n")
print(f"{len(packages)} packages, {len(missing)} with no licence file: {', '.join(missing)}")
