#!/usr/bin/env python3
# Writes licenses/rust-crates.txt: every crate compiled into the macOS exe (normal dependencies
# for aarch64-apple-darwin, without proc macros and what only they use), its version and licence,
# and, where that licence asks for its text and copyright lines to go along, the crate's own
# licence files. A crate that offers Apache 2.0 is used under it (licenses/Apache-2.0.txt
# covers them all, plus any NOTICE file the crate has); one that offers the Unlicense or 0BSD
# needs nothing, and nor does the WTFPL. A crate published without its MIT licence file gets
# the MIT text with its authors as the copyright holders. Run it again whenever Cargo.lock
# changes.
#
#   python3 scripts/crate-licenses.py
import json
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parent.parent
meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--locked",
     "--filter-platform", "aarch64-apple-darwin", "--manifest-path", str(root / "Cargo.toml")],
    check=True, capture_output=True, text=True).stdout)
packages = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}


def proc_macro(package):
    return any("proc-macro" in t["kind"] for t in package["targets"])


# Walk the normal dependencies from wushang; proc macros run in the compiler, not in the app.
compiled, todo = set(), [meta["resolve"]["root"]]
while todo:
    for dep in nodes[todo.pop()]["deps"]:
        normal = any(k["kind"] is None for k in dep["dep_kinds"])
        if normal and dep["pkg"] not in compiled and not proc_macro(packages[dep["pkg"]]):
            compiled.add(dep["pkg"])
            todo.append(dep["pkg"])

# The licences that need nothing past licenses/Apache-2.0.txt, best first.
NO_TEXT = ["Apache-2.0", "Unlicense", "0BSD", "WTFPL"]


def choice(expression):
    """The licence a crate is used under, and whether its own licence files must ship."""
    if " AND " in expression:
        return expression, True
    options = [o.strip("() ") for o in re.split(r" OR |/", expression)]
    for licence in NO_TEXT:
        if licence in options:
            return licence, False
    return options[0], True


MIT = """Permission is hereby granted, free of charge, to any person obtaining a copy of this
software and associated documentation files (the "Software"), to deal in the Software without
restriction, including without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE."""

lines, texts, missing = [], [], []
for package in sorted((packages[i] for i in compiled), key=lambda p: (p["name"], p["version"])):
    expression = package.get("license") or "see its licence file"
    used, ships_text = choice(expression)
    lines.append(f"{package['name']} {package['version']}: {expression}"
                 + (f" (used under {used})" if used != expression else ""))
    folder = pathlib.Path(package["manifest_path"]).parent
    # Apache 2.0 also asks for a NOTICE file to go along, when the crate has one.
    pattern = r"(LICEN[CS]E|COPYING|NOTICE)" if ships_text else r"NOTICE"
    files = sorted(f for f in folder.iterdir() if f.is_file() and re.match(pattern, f.name, re.I))
    if ships_text and not files:
        if used != "MIT":
            missing.append(package["name"])
            continue
        holders = ", ".join(re.sub(r"\s*<[^>]*>", "", a) for a in package["authors"])
        texts.append(f"==== {package['name']} {package['version']}: MIT (the crate ships no "
                     f"licence file) ====\n\nCopyright (c) {holders or 'the ' + package['name'] + ' authors'}\n\n{MIT}\n")
    for f in files:
        texts.append(f"==== {package['name']} {package['version']}: {f.name} ====\n\n"
                     + f.read_text(errors="replace").strip() + "\n")

out = root / "licenses" / "rust-crates.txt"
out.write_text(
    "Rust crates compiled into wushang (macOS, arm64), from Cargo.lock by\n"
    "scripts/crate-licenses.py. Crates that offer Apache 2.0 are used under it (Apache-2.0.txt).\n"
    "Below the list are the licence files of those whose licences ask for them.\n\n"
    + "\n".join(lines) + "\n\n" + "\n".join(texts))
print(f"{out}: {len(lines)} crates, {len(texts)} licence and notice files")
if missing:
    sys.exit(f"no licence file in: {', '.join(missing)}")
