# PeerBeam v0.8.1 — Beta

A packaging release. No application code changed; what changed is which Linux
packages exist and whether they carry an icon.

## Every Linux distribution has a package again

**`.rpm` — Fedora, RHEL, Rocky, Alma, CentOS Stream, openSUSE.** The build
script's rpm branch printed *"rpmbuild present — see docs/BUILD.md for the .spec
flow"* and built nothing, beneath a header claiming it produced one when the
tool was present. CI installs `rpm`, so it took that branch every release and
shipped no package. Those distributions have only ever had the tarball. There is
now a real `.rpm`, built from the same staged tree as the `.deb` so the two
cannot describe different layouts.

**AppImage — any distribution, installs nothing.** The branch was real code, but
CI never installed `appimagetool`, so the one artifact that runs anywhere was
never built. It is built now, and runs without FUSE via
`--appimage-extract-and-run`.

**Arch, Manjaro, EndeavourOS** — `packaging/arch/PKGBUILD` builds a native
package from the release tarball rather than from source, so the installed
binary is the one that was tested.

## The app icon exists now

Every published Linux package shipped a desktop entry pointing at an icon that
was not there. CI installed `librsvg2-bin` as its rasterizer, but the icon
master is a PNG needing *resizing* and the script looks for `magick`/`convert` —
librsvg renders SVGs and provides neither, so every build logged *"no
rasterizer; icons will be missing"* and carried on. It went unnoticed because
ImageMagick is present on a typical development machine: local builds carried
all five sizes, CI's carried none, and only CI's are published.

## Manual release runs work

`package-linux.sh` took the version from `GITHUB_REF_NAME`, which is a tag on a
tag push but the *branch* under `workflow_dispatch`. A manual run therefore
built `peerbeam-main-linux-x64.tar.gz` and handed dpkg a control file reading
`Version: main`, which it refused. Every manual run had died there — which is
also why the rpm stub survived so long: the natural way to test packaging never
reached it.

## Downloads

Linux now ships four ways: **`.deb`**, **`.rpm`**, **`.AppImage`** and the
portable **`.tar.gz`**. Windows (portable `.zip`), macOS (universal `.dmg`),
Android (`.apk`/`.aab`) and the CLI for Linux, macOS and Windows are attached as
before. Everything is **unsigned** — see [GUIDE.md](GUIDE.md) for the
per-platform steps, including Gatekeeper and SmartScreen.

Full detail in [CHANGELOG.md](../CHANGELOG.md).
