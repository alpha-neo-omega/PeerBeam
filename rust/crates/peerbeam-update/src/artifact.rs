//! Which published file *this* build should fetch for itself.
//!
//! # Why this is not a lookup in the manifest
//!
//! Amendment A3's second binding condition:
//!
//! > **The artifact URL is compiled in, never served.** The manifest may say
//! > *which version* is newest. It may not say *where to fetch anything*. The
//! > artifact address is constructed in the app from a compiled-in host, the
//! > version string, and the platform's known asset-name pattern.
//!
//! So the naming rules live here, in the client, as a pure function. A served
//! document cannot choose what lands on a user's disk, which is the property
//! the release check already protects by ignoring the manifest's `url` field.
//!
//! # The names are the packaging scripts' names
//!
//! They are not guessed from one observed release. Every pattern below comes
//! from the script that generates it, and the arch spellings are the reason
//! this is a function with tests rather than a format string at the call site:
//!
//! | Format | Script | x86-64 | aarch64 |
//! |---|---|---|---|
//! | `.deb` | `package-linux.sh:158` | `amd64` | `arm64` |
//! | `.rpm` | `package-linux.sh:225` | `x86_64` | `aarch64` |
//! | `.AppImage` | `package-linux.sh:255` | `x86_64` | `aarch64` |
//! | `.tar.gz` | `package-linux.sh:139` | `x64` | `arm64` |
//! | `.zip` | `package-windows.ps1:44` | `x64` | `arm64` |
//! | `.dmg` | `package-macos.sh:68` | universal — no arch in the name |
//!
//! Three spellings of one architecture across four Linux formats, because each
//! ecosystem names it its own way and the scripts follow each convention
//! rather than inventing a fifth. `package-linux.sh:31` says as much.
//!
//! # Refusing is a normal outcome
//!
//! A Linux machine can hold `.deb`, `.rpm`, `.AppImage` and `.tar.gz` builds of
//! the same release, and nothing in a running process says which one a user
//! installed unless the system can be asked. Handing someone the wrong format
//! is the failure this feature exists to prevent, so where the answer is not
//! knowable this refuses and leaves them the website — which is exactly where
//! they are today.

use std::path::Path;

/// The operating systems a release is published for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Any Linux, whatever the packaging.
    Linux,
    /// Windows, shipped as the portable zip.
    Windows,
    /// macOS, shipped as one universal DMG.
    MacOs,
}

/// The architectures a release is published for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    /// x86-64 / amd64 / x64, depending on who is spelling it.
    X86_64,
    /// aarch64 / arm64.
    Aarch64,
}

/// How a Linux copy of PeerBeam was installed.
///
/// There is deliberately no `TarGz` variant. A tarball install is
/// indistinguishable from a build from source, a Nix profile, or a copy
/// someone moved into place by hand — see [`detect_linux_format`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxFormat {
    /// dpkg owns the running binary.
    Deb,
    /// rpm owns the running binary.
    Rpm,
    /// The process is running from an AppImage.
    AppImage,
}

/// Why no artifact could be named for this build.
///
/// Every variant means *do not download anything*; the caller sends the person
/// to the download page, which is what happens today.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResolveError {
    /// Linux, and which package this is cannot be established.
    #[error(
        "cannot tell which Linux package this copy of PeerBeam was installed from, so there is no way to know which file to fetch"
    )]
    UnknownLinuxFormat,

    /// Linux, and more than one package manager claims the running binary.
    #[error(
        "more than one package manager claims this copy of PeerBeam ({0}), so which file to fetch is ambiguous"
    )]
    AmbiguousLinuxFormat(String),

    /// An OS with no desktop release, or one this build does not know.
    ///
    /// Android is the live case: it ships through an APK/AAB and its own
    /// install flow, not this.
    #[error("{0} has no release artifact this build knows how to fetch")]
    UnsupportedOs(String),

    /// An architecture with no published build.
    #[error("no release is published for this machine's architecture")]
    UnsupportedArch,

    /// The version string is empty or not a version.
    #[error("cannot build an artifact name for an empty version")]
    NoVersion,
}

/// The basename the release publishes for this combination.
///
/// Pure: no environment, no filesystem, no network. Every pattern is testable
/// against the packaging scripts without running any of them.
///
/// `linux` is required when `os` is [`Os::Linux`] and ignored otherwise — the
/// caller establishes it with [`detect_linux_format`], which is the part that
/// can fail.
pub fn asset_name(
    os: Os,
    arch: Arch,
    linux: Option<LinuxFormat>,
    version: &str,
) -> Result<String, ResolveError> {
    let v = version.trim().trim_start_matches('v');
    if v.is_empty() {
        return Err(ResolveError::NoVersion);
    }
    Ok(match os {
        // One universal DMG, both architectures inside it, so `arch` does not
        // appear -- `package-macos.sh:68`.
        Os::MacOs => format!("PeerBeam-{v}.dmg"),
        Os::Windows => {
            let a = match arch {
                Arch::X86_64 => "x64",
                Arch::Aarch64 => "arm64",
            };
            format!("peerbeam-{v}-windows-{a}-portable.zip")
        }
        Os::Linux => {
            let format = linux.ok_or(ResolveError::UnknownLinuxFormat)?;
            match format {
                // Debian's spelling.
                LinuxFormat::Deb => {
                    let a = match arch {
                        Arch::X86_64 => "amd64",
                        Arch::Aarch64 => "arm64",
                    };
                    format!("peerbeam-{v}-{a}.deb")
                }
                // RPM's and AppImage's spelling, which happen to agree.
                LinuxFormat::Rpm | LinuxFormat::AppImage => {
                    let a = match arch {
                        Arch::X86_64 => "x86_64",
                        Arch::Aarch64 => "aarch64",
                    };
                    let ext = if format == LinuxFormat::Rpm {
                        "rpm"
                    } else {
                        "AppImage"
                    };
                    format!("peerbeam-{v}-{a}.{ext}")
                }
            }
        }
    })
}

/// This build's OS and architecture, from what it was compiled for.
///
/// `cfg!` rather than anything runtime: an x86-64 binary running under Rosetta
/// or Windows-on-ARM emulation should fetch the build it *is*, not the build
/// the host could run. Fetching the "native" one would hand someone a package
/// their working install cannot be replaced by.
pub fn this_target() -> Result<(Os, Arch), ResolveError> {
    let os = if cfg!(target_os = "linux") {
        Os::Linux
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::MacOs
    } else {
        return Err(ResolveError::UnsupportedOs(
            std::env::consts::OS.to_string(),
        ));
    };
    // Android is Linux by `target_os`, and must not be treated as one: it
    // installs an APK through its own flow and has no desktop artifact.
    if cfg!(target_os = "android") {
        return Err(ResolveError::UnsupportedOs("android".to_string()));
    }
    let arch = if cfg!(target_arch = "x86_64") {
        Arch::X86_64
    } else if cfg!(target_arch = "aarch64") {
        Arch::Aarch64
    } else {
        return Err(ResolveError::UnsupportedArch);
    };
    Ok((os, arch))
}

/// Ask the system which package owns the running binary.
///
/// Blocking: it runs `dpkg -S` / `rpm -qf`. Call it off the async runtime.
///
/// # How it decides
///
/// 1. `$APPIMAGE` is set by the AppImage runtime itself and names the image,
///    so its presence is definitive and needs nothing else asked.
/// 2. Otherwise the package managers present are each asked whether they own
///    the running executable. Ownership is the only authoritative answer;
///    guessing from the install path is not — `/usr/bin` is where a `.deb`, an
///    `.rpm` and `make install` all put things.
/// 3. Exactly one claim wins. Two claims are [`ResolveError::AmbiguousLinuxFormat`].
/// 4. No claim is [`ResolveError::UnknownLinuxFormat`], **including the
///    tarball case**. A tarball install looks identical to a build from
///    source, a Nix or Homebrew copy, and a binary someone moved into place.
///    Assuming "no package manager means tarball" would hand a from-source user
///    a tarball to overwrite their build with.
pub fn detect_linux_format() -> Result<LinuxFormat, ResolveError> {
    if std::env::var_os("APPIMAGE").is_some() {
        return Ok(LinuxFormat::AppImage);
    }
    let exe = std::env::current_exe().map_err(|_| ResolveError::UnknownLinuxFormat)?;
    // Resolve symlinks: `/usr/bin/peerbeam` may point into `/usr/lib`, and a
    // package manager knows the file it installed, not the link to it.
    let exe = exe.canonicalize().unwrap_or(exe);
    let mut claims: Vec<LinuxFormat> = Vec::new();
    if owns(&["dpkg", "-S"], &exe) {
        claims.push(LinuxFormat::Deb);
    }
    if owns(&["rpm", "-qf"], &exe) {
        claims.push(LinuxFormat::Rpm);
    }
    match claims.len() {
        1 => Ok(claims[0]),
        0 => Err(ResolveError::UnknownLinuxFormat),
        _ => Err(ResolveError::AmbiguousLinuxFormat(
            claims
                .iter()
                .map(|c| format!("{c:?}"))
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

/// Whether `tool` reports that it owns `path`.
///
/// A non-zero exit, a missing tool, or a failure to spawn all mean "no". None
/// of them is an error worth surfacing: a machine without `rpm` is not broken,
/// it is a machine without `rpm`.
fn owns(tool: &[&str], path: &Path) -> bool {
    let Some((bin, args)) = tool.split_first() else {
        return false;
    };
    std::process::Command::new(bin)
        .args(args)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The basename this build should fetch for the given version.
///
/// Blocking on Linux, for the reason given on [`detect_linux_format`].
pub fn artifact_for_this_build(version: &str) -> Result<String, ResolveError> {
    let (os, arch) = this_target()?;
    let linux = if os == Os::Linux {
        Some(detect_linux_format()?)
    } else {
        None
    };
    asset_name(os, arch, linux, version)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v0.12.0 release's actual asset list, for the desktop artifacts. If
    /// a packaging script ever renames one of these, the assertions below stop
    /// matching reality and this is where it shows up.
    const PUBLISHED: &[&str] = &[
        "PeerBeam-0.12.0.dmg",
        "peerbeam-0.12.0-aarch64.AppImage",
        "peerbeam-0.12.0-aarch64.rpm",
        "peerbeam-0.12.0-amd64.deb",
        "peerbeam-0.12.0-arm64.deb",
        "peerbeam-0.12.0-linux-arm64.tar.gz",
        "peerbeam-0.12.0-linux-x64.tar.gz",
        "peerbeam-0.12.0-windows-arm64-portable.zip",
        "peerbeam-0.12.0-windows-x64-portable.zip",
        "peerbeam-0.12.0-x86_64.AppImage",
        "peerbeam-0.12.0-x86_64.rpm",
    ];

    fn name(os: Os, arch: Arch, linux: Option<LinuxFormat>) -> String {
        asset_name(os, arch, linux, "0.12.0").expect("should resolve")
    }

    /// The point of the whole module: every name it produces is a file the
    /// release actually published. A typo here is a 404 for a user who was
    /// told an update exists.
    #[test]
    fn every_name_produced_is_a_file_the_release_published() {
        let cases = [
            (Os::MacOs, Arch::X86_64, None),
            (Os::MacOs, Arch::Aarch64, None),
            (Os::Windows, Arch::X86_64, None),
            (Os::Windows, Arch::Aarch64, None),
            (Os::Linux, Arch::X86_64, Some(LinuxFormat::Deb)),
            (Os::Linux, Arch::Aarch64, Some(LinuxFormat::Deb)),
            (Os::Linux, Arch::X86_64, Some(LinuxFormat::Rpm)),
            (Os::Linux, Arch::Aarch64, Some(LinuxFormat::Rpm)),
            (Os::Linux, Arch::X86_64, Some(LinuxFormat::AppImage)),
            (Os::Linux, Arch::Aarch64, Some(LinuxFormat::AppImage)),
        ];
        for (os, arch, linux) in cases {
            let n = name(os, arch, linux);
            assert!(
                PUBLISHED.contains(&n.as_str()),
                "{os:?}/{arch:?}/{linux:?} produced {n}, which the release does not publish"
            );
        }
    }

    /// Each architecture's three Linux spellings, which is the mistake this
    /// module exists to make impossible.
    #[test]
    fn each_linux_format_uses_its_own_ecosystems_arch_spelling() {
        assert_eq!(
            name(Os::Linux, Arch::X86_64, Some(LinuxFormat::Deb)),
            "peerbeam-0.12.0-amd64.deb"
        );
        assert_eq!(
            name(Os::Linux, Arch::X86_64, Some(LinuxFormat::Rpm)),
            "peerbeam-0.12.0-x86_64.rpm"
        );
        assert_eq!(
            name(Os::Linux, Arch::X86_64, Some(LinuxFormat::AppImage)),
            "peerbeam-0.12.0-x86_64.AppImage"
        );
    }

    #[test]
    fn aarch64_spellings_differ_the_same_way() {
        assert_eq!(
            name(Os::Linux, Arch::Aarch64, Some(LinuxFormat::Deb)),
            "peerbeam-0.12.0-arm64.deb"
        );
        assert_eq!(
            name(Os::Linux, Arch::Aarch64, Some(LinuxFormat::Rpm)),
            "peerbeam-0.12.0-aarch64.rpm"
        );
    }

    /// One DMG covers both architectures, so the name carries neither.
    #[test]
    fn macos_gets_one_universal_dmg_for_both_architectures() {
        assert_eq!(
            name(Os::MacOs, Arch::X86_64, None),
            name(Os::MacOs, Arch::Aarch64, None)
        );
        assert_eq!(name(Os::MacOs, Arch::X86_64, None), "PeerBeam-0.12.0.dmg");
    }

    /// The DMG is the one asset with a capitalised name.
    #[test]
    fn the_dmg_keeps_its_capitalisation() {
        assert!(name(Os::MacOs, Arch::X86_64, None).starts_with("PeerBeam-"));
        assert!(name(Os::Windows, Arch::X86_64, None).starts_with("peerbeam-"));
    }

    #[test]
    fn windows_ships_the_portable_zip() {
        assert_eq!(
            name(Os::Windows, Arch::X86_64, None),
            "peerbeam-0.12.0-windows-x64-portable.zip"
        );
        assert_eq!(
            name(Os::Windows, Arch::Aarch64, None),
            "peerbeam-0.12.0-windows-arm64-portable.zip"
        );
    }

    /// Linux with no established format refuses rather than picking one.
    #[test]
    fn linux_without_a_known_format_refuses() {
        assert_eq!(
            asset_name(Os::Linux, Arch::X86_64, None, "0.12.0"),
            Err(ResolveError::UnknownLinuxFormat)
        );
    }

    /// A leading `v` is not part of the name, matching `newest`'s handling.
    #[test]
    fn a_leading_v_is_stripped() {
        assert_eq!(
            asset_name(Os::MacOs, Arch::X86_64, None, "v0.12.0").unwrap(),
            "PeerBeam-0.12.0.dmg"
        );
    }

    #[test]
    fn an_empty_version_is_refused() {
        assert_eq!(
            asset_name(Os::MacOs, Arch::X86_64, None, "  "),
            Err(ResolveError::NoVersion)
        );
    }

    /// Every refusal says something a person could act on, and none of them
    /// reads as "carry on".
    #[test]
    fn refusals_explain_themselves() {
        for e in [
            ResolveError::UnknownLinuxFormat,
            ResolveError::AmbiguousLinuxFormat("Deb, Rpm".into()),
            ResolveError::UnsupportedOs("android".into()),
            ResolveError::UnsupportedArch,
            ResolveError::NoVersion,
        ] {
            let m = e.to_string();
            assert!(m.len() > 20, "{m} is too terse to act on");
        }
    }

    /// An AppImage announces itself, so detection needs nothing else.
    ///
    /// Not asserted by setting `$APPIMAGE` and calling the real function: the
    /// environment is process-wide and Rust runs tests in threads, so that
    /// would race any other test reading the environment. The branch is one
    /// `var_os` check; what is worth pinning is that AppImage and rpm share an
    /// arch spelling but not an extension.
    #[test]
    fn appimage_and_rpm_differ_only_in_extension() {
        let app = name(Os::Linux, Arch::X86_64, Some(LinuxFormat::AppImage));
        let rpm = name(Os::Linux, Arch::X86_64, Some(LinuxFormat::Rpm));
        assert_eq!(
            app.trim_end_matches(".AppImage"),
            rpm.trim_end_matches(".rpm")
        );
        assert_ne!(app, rpm);
    }

    /// The R2 mirror uploads a subset of the release, chosen by glob patterns
    /// in `.github/workflows/release.yml`. This module decides what the app
    /// asks for. Those two lists are in different languages in different files
    /// and nothing but this test connects them — and a resolver naming a file
    /// the mirror never uploaded is a 404 handed to someone who was just told
    /// an update exists.
    ///
    /// Reads the workflow rather than restating its patterns, so the test
    /// fails when the workflow changes rather than when someone remembers to
    /// update a copy of it here.
    #[test]
    fn the_r2_mirror_uploads_every_name_this_module_can_ask_for() {
        let workflow = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../.github/workflows/release.yml");
        let text = std::fs::read_to_string(&workflow)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", workflow.display()));

        // The line is marked in the workflow precisely so this can find it.
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with("patterns=("))
            .expect("no `patterns=(` line in release.yml — did the mirror step change?");
        let inner = line
            .split_once('(')
            .and_then(|(_, r)| r.rsplit_once(')'))
            .map(|(m, _)| m)
            .expect("malformed patterns=( ... ) line");
        let patterns: Vec<&str> = inner
            .split_whitespace()
            .map(|p| p.trim_matches('\''))
            .filter(|p| !p.is_empty())
            .collect();
        assert!(!patterns.is_empty(), "the mirror uploads nothing");

        for name in PUBLISHED_BY_THIS_MODULE {
            assert!(
                patterns.iter().any(|p| glob_matches(p, name)),
                "{name} is fetched by the app but no mirror pattern in release.yml matches it \
                 (patterns: {patterns:?})"
            );
        }
        // The verification inputs travel with the artifacts or nothing can be
        // checked, and they are named exactly, not globbed.
        for required in ["SHA256SUMS", "SHA256SUMS.minisig"] {
            assert!(
                patterns.iter().any(|p| glob_matches(p, required)),
                "{required} is not mirrored, so no download could ever be verified"
            );
        }
    }

    /// Every name [`asset_name`] can produce, for a representative version.
    const PUBLISHED_BY_THIS_MODULE: &[&str] = &[
        "PeerBeam-0.12.0.dmg",
        "peerbeam-0.12.0-windows-x64-portable.zip",
        "peerbeam-0.12.0-windows-arm64-portable.zip",
        "peerbeam-0.12.0-amd64.deb",
        "peerbeam-0.12.0-arm64.deb",
        "peerbeam-0.12.0-x86_64.rpm",
        "peerbeam-0.12.0-aarch64.rpm",
        "peerbeam-0.12.0-x86_64.AppImage",
        "peerbeam-0.12.0-aarch64.AppImage",
    ];

    /// `*` matches any run of characters; everything else is literal. Enough
    /// for the shell globs the workflow uses, and not a dependency.
    fn glob_matches(pattern: &str, name: &str) -> bool {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 1 {
            return pattern == name;
        }
        let mut rest = name;
        // The first segment must sit at the start, the last at the end.
        if let Some(first) = parts.first() {
            match rest.strip_prefix(first) {
                Some(r) => rest = r,
                None => return false,
            }
        }
        if let Some(last) = parts.last() {
            if !rest.ends_with(last) || rest.len() < last.len() {
                return false;
            }
            rest = &rest[..rest.len() - last.len()];
        }
        for mid in &parts[1..parts.len() - 1] {
            match rest.find(mid) {
                Some(i) => rest = &rest[i + mid.len()..],
                None => return false,
            }
        }
        true
    }

    #[test]
    fn the_glob_matcher_behaves() {
        assert!(glob_matches("*.deb", "peerbeam-0.12.0-amd64.deb"));
        assert!(!glob_matches("*.deb", "peerbeam-0.12.0-amd64.rpm"));
        assert!(glob_matches("SHA256SUMS", "SHA256SUMS"));
        assert!(!glob_matches("SHA256SUMS", "SHA256SUMS.minisig"));
        assert!(glob_matches("PeerBeam-*.dmg", "PeerBeam-0.12.0.dmg"));
        assert!(!glob_matches("PeerBeam-*.dmg", "peerbeam-0.12.0.dmg"));
        assert!(glob_matches(
            "*-portable.zip",
            "peerbeam-0.12.0-windows-x64-portable.zip"
        ));
        assert!(!glob_matches(
            "*-portable.zip",
            "peerbeam-0.12.0-android.apk"
        ));
    }

    /// Whatever this machine is, resolving must either produce a published
    /// name or refuse with a reason — never panic, and never an empty string.
    #[test]
    fn this_build_resolves_or_refuses_cleanly() {
        match artifact_for_this_build("0.12.0") {
            Ok(n) => assert!(
                PUBLISHED.contains(&n.as_str()),
                "{n} is not a published asset"
            ),
            Err(e) => assert!(!e.to_string().is_empty()),
        }
    }
}
