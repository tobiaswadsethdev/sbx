//! Updating `sbx` itself, and the `sbxd` beside it.
//!
//! The tool that installs it is `install.sh`, which needs no checkout and no
//! Rust toolchain: it fetches the newest release, checks it against the
//! published SHA-256, and drops the binary into a directory on `PATH`. This
//! module is the same three steps performed by the binary already installed, so
//! updating is `sbx update` rather than remembering the curl line.
//!
//! **Explicit, never in the background.** Nothing here runs on a timer and
//! nothing installs without being asked: [`check`] is what `sbx doctor` calls,
//! and it only ever *reports*. A tool whose whole claim is that it can say what
//! is running inside a sandbox has no business replacing its own binary while
//! nobody is looking -- and an agent mid-session is the worst possible moment to
//! find out that the thing polling it changed version.
//!
//! Through `curl`, `sha256sum` and `tar` rather than an HTTP client, a hashing
//! crate and a decompressor, for the reason [`crate::image::latest_claude_version`]
//! gives: the whole project is built on subprocesses, and this is the only place
//! that would want a TLS stack.
//!
//! The three files that have to agree about what a release is called, and
//! about what is inside it -- `install.sh`, `.github/workflows/release.yml`
//! and this one -- are kept in step by tests at the bottom rather than by
//! anyone remembering. From v0.3.1 the archive carries `sbx` *and* `sbxd`,
//! because the server used to be reachable only through `cargo install`, on
//! the machine whose whole point is not needing a Rust toolchain.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::image::is_older;

/// Where releases are published.
pub const REPO: &str = "tobiaswadsethdev/sbx";

/// The version of the running binary.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The release this build of `sbx` would install for itself.
///
/// `None` on a platform no release is built for, which is every platform but
/// Linux: the isolation is kernel-enforced, so there is nothing to run
/// elsewhere, and a musl build runs on any distribution without asking about
/// its libc.
fn target_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

fn target() -> Option<&'static str> {
    target_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// What a release asset is called. `install.sh` builds the same name.
fn asset_name(tag: &str, target: &str) -> String {
    format!("sbx-{tag}-{target}.tar.gz")
}

/// The checksum file covering every asset in a release.
const SUMS: &str = "SHA256SUMS";

/// The client, and the binary this module is usually replacing.
const CLIENT: &str = "sbx";

/// The server, which rides in the same archive from v0.3.1 on.
const SERVER: &str = "sbxd";

/// A published release, as much of one as this needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The git tag, `v0.1.0`.
    pub tag: String,
    /// The tag without its `v`, comparable with [`current`].
    pub version: String,
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Asset {
    name: String,
    url: String,
}

impl Release {
    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// The GitHub API's shape, narrowed to what is used.
#[derive(serde::Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(serde::Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

fn parse_release(json: &str) -> Option<Release> {
    let api: ApiRelease = serde_json::from_str(json).ok()?;
    let version = api.tag_name.strip_prefix('v').unwrap_or(&api.tag_name);
    // A tag that is not a version is not something this can compare against, so
    // it is better ignored than reported as an update.
    if !version.contains('.') {
        return None;
    }
    Some(Release {
        version: version.to_string(),
        tag: api.tag_name.clone(),
        assets: api
            .assets
            .into_iter()
            .map(|a| Asset {
                name: a.name,
                url: a.browser_download_url,
            })
            .collect(),
    })
}

/// GET a URL through `curl`, with timeouts short enough that no caller is left
/// waiting on a network that is not answering.
fn fetch(url: &str) -> Option<String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--connect-timeout",
            "3",
            "--max-time",
            "20",
            "-H",
            "Accept: application/vnd.github+json",
            url,
        ])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// The newest published release.
///
/// `None` means "could not ask" -- no releases yet, no network, a rate limit --
/// and no caller may read it as "up to date".
pub fn latest() -> Option<Release> {
    parse_release(&fetch(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))?)
}

/// One named release, for going back to a version that worked.
fn tagged(tag: &str) -> Option<Release> {
    parse_release(&fetch(&format!(
        "https://api.github.com/repos/{REPO}/releases/tags/{tag}"
    ))?)
}

/// What an update would do, without doing any of it.
#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    /// The newest release is what is already running.
    Current(String),
    /// A newer release exists.
    Newer { running: String, latest: String },
    /// The running binary is ahead of the newest release, which is what a
    /// build from a checkout looks like. Not something to report as a problem.
    Ahead(String),
    /// The release list could not be read. Never "up to date".
    Unknown,
}

/// Compare the running binary against the newest release.
pub fn check() -> Status {
    match latest() {
        None => Status::Unknown,
        Some(r) if is_older(current(), &r.version) => Status::Newer {
            running: current().to_string(),
            latest: r.version,
        },
        Some(r) if r.version == current() => Status::Current(r.version),
        Some(_) => Status::Ahead(current().to_string()),
    }
}

/// What [`install`] did.
pub enum Outcome {
    NoChange {
        version: String,
    },
    Updated {
        from: String,
        to: String,
        at: PathBuf,
        /// Where the `sbxd` beside it was replaced, if there was one. A
        /// running server keeps the old inode until it is restarted, so a
        /// caller that reports this should say so.
        sbxd: Option<PathBuf>,
    },
}

/// Fetch a release and put it where the running binary is.
///
/// `tag` names a specific release; `None` takes the newest. `force` reinstalls
/// a version that is already running, which is the difference between "nothing
/// to do" and "this binary is damaged, fetch it again".
pub fn install(tag: Option<&str>, force: bool) -> Result<Outcome, String> {
    let release = match tag {
        Some(t) => tagged(t).ok_or_else(|| {
            format!("no release tagged `{t}`, or github could not be reached\n     fix: `sbx update` takes the newest; see https://github.com/{REPO}/releases")
        })?,
        None => latest().ok_or_else(|| {
            format!("could not read the release list -- there may be none published yet\n     fix: build from source with `cargo install --git https://github.com/{REPO} sbx --locked`")
        })?,
    };

    if release.version == current() && !force {
        return Ok(Outcome::NoChange {
            version: release.version,
        });
    }

    let target = target().ok_or_else(|| {
        format!(
            "no release is built for {} {}\n     fix: build from source with `cargo install --git https://github.com/{REPO} sbx --locked`",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;

    let name = asset_name(&release.tag, target);
    let asset = release
        .asset(&name)
        .ok_or_else(|| format!("release {} has no {name}", release.tag))?;
    // An unverified binary is not installed, so a release without its checksums
    // is a release this refuses rather than trusts.
    let sums = release.asset(SUMS).ok_or_else(|| {
        format!(
            "release {} publishes no {SUMS}, so nothing can be verified",
            release.tag
        )
    })?;

    let dir = std::env::temp_dir().join(format!("sbx-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let result = stage_and_swap(&dir, asset, sums, &name, &release);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Download, verify, unpack, and put the new binary in place.
///
/// Split out so the temporary directory is cleaned up on every path out,
/// including the failures.
fn stage_and_swap(
    dir: &Path,
    asset: &Asset,
    sums: &Asset,
    name: &str,
    release: &Release,
) -> Result<Outcome, String> {
    let tarball = dir.join(name);
    download(&asset.url, &tarball)?;
    let sums_path = dir.join(SUMS);
    download(&sums.url, &sums_path)?;

    let published =
        std::fs::read_to_string(&sums_path).map_err(|e| format!("{}: {e}", sums_path.display()))?;
    let expected =
        expected_sha(&published, name).ok_or_else(|| format!("{SUMS} does not cover {name}"))?;
    let actual = sha256(&tarball)?;
    if actual != expected {
        // Not a retry: a mismatch is either a corrupted download or a swapped
        // asset, and neither is fixed by fetching it again.
        return Err(format!(
            "checksum mismatch for {name}\n       expected {expected}\n       got      {actual}\n     fix: report this at https://github.com/{REPO}/security/advisories/new"
        ));
    }

    let status = Command::new("tar")
        .args(["-xzf"])
        .arg(&tarball)
        .arg("-C")
        .arg(dir)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err(format!("could not unpack {name}"));
    }
    let fresh = dir.join(CLIENT);
    if !fresh.is_file() {
        return Err(format!("{name} does not contain an `{CLIENT}` binary"));
    }
    make_executable(&fresh)?;

    // Ask the downloaded binary what it is before trusting it with the path the
    // running one occupies. A release whose asset was built from the wrong
    // commit answers the wrong version here, which is cheaper to find out now
    // than after the swap.
    let reported = Command::new(&fresh)
        .arg("--version")
        .output()
        .map_err(|e| format!("the downloaded binary does not run: {e}"))?;
    let reported = String::from_utf8_lossy(&reported.stdout);
    let reported = reported.split_whitespace().nth(1).unwrap_or("");
    if reported != release.version {
        return Err(format!(
            "the downloaded binary reports {reported}, not {}",
            release.version
        ));
    }

    let at = std::env::current_exe().map_err(|e| format!("cannot find the running binary: {e}"))?;
    swap(&fresh, &at)?;
    // The server half, after the client and not before it: if replacing `sbxd`
    // fails, the failure is reported against an `sbx` that is already the new
    // version, which is a state worth having rather than one to unpick.
    let sbxd = swap_server(dir, &at)?;
    Ok(Outcome::Updated {
        from: current().to_string(),
        to: release.version.clone(),
        at,
        sbxd,
    })
}

/// Replace the `sbxd` beside `sbx`, when there is one to replace.
///
/// Only when it is *already* installed there. `sbx update` is asked to update
/// what this machine has, and putting a server binary on a machine whose
/// operator never installed one is a decision they did not make -- on a laptop
/// that only ever drives local sandboxes, `sbxd` is a listening socket nobody
/// asked for.
///
/// Skipped without complaint when the archive has no `sbxd` in it, which is
/// every release up to v0.3.0. Going back to one of those with
/// `sbx update --tag` leaves the newer server in place, and that is survivable:
/// compatibility between the two is decided by the protocol number in
/// `sbx-proto`, which moves far more slowly than the crate version, so a
/// version skew between them is usually no skew at all.
fn swap_server(dir: &Path, client_at: &Path) -> Result<Option<PathBuf>, String> {
    let fresh = dir.join(SERVER);
    let at = client_at.with_file_name(SERVER);
    if !fresh.is_file() || !at.is_file() {
        return Ok(None);
    }
    make_executable(&fresh)?;
    swap(&fresh, &at)?;
    Ok(Some(at))
}

/// Put `fresh` where `at` is, atomically.
///
/// Through a sibling of the target rather than a rename out of the temporary
/// directory: `/tmp` is very often a different filesystem, and `rename` across
/// one fails with `EXDEV`. Copying next door and renaming means the moment of
/// replacement is a single atomic operation, so a torn write cannot leave a
/// half-written `sbx` on the path -- and Linux is happy to rename over a binary
/// that is currently executing, which is what makes this possible at all.
fn swap(fresh: &Path, at: &Path) -> Result<(), String> {
    let dir = at.parent().unwrap_or(Path::new("."));
    // Named after the target, because this now stages two different binaries
    // into the same directory and a shared name would make a half-finished
    // update of one look like a half-finished update of the other.
    let name = at.file_name().map_or("sbx".into(), |n| n.to_string_lossy());
    let staged = dir.join(format!(".{name}-update-{}", std::process::id()));
    std::fs::copy(fresh, &staged).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!(
            "cannot write to {}: {e}\n     fix: re-run with sudo, or install into a directory you own",
            dir.display()
        )
    })?;
    make_executable(&staged)?;
    std::fs::rename(&staged, at).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("cannot replace {}: {e}", at.display())
    })
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Nothing to do: Windows takes "executable" from the extension, not from a
/// bit on the file. Reachable only because this crate compiles for Windows for
/// the desktop application's sake -- there are no Windows releases of `sbx` to
/// download and no `sbx update` there to download one.
#[cfg(windows)]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    let status = Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "3", "--max-time", "300", "-o"])
        .arg(to)
        .arg(url)
        .status()
        .map_err(|e| format!("curl: {e}"))?;
    if !status.success() {
        return Err(format!("could not download {url}"));
    }
    Ok(())
}

fn sha256(path: &Path) -> Result<String, String> {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|e| format!("sha256sum: {e}"))?;
    if !out.status.success() {
        return Err(format!("could not hash {}", path.display()));
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| "sha256sum said nothing".to_string())
}

/// The hash `sums` publishes for `name`.
///
/// `sha256sum` writes `<hash>  <name>` for a text read and `<hash> *<name>` for
/// a binary one, so the marker is stripped rather than matched on.
fn expected_sha(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        let file = file.trim().trim_start_matches('*');
        (file == name).then(|| hash.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The API shape this parses, trimmed to the keys that are read. Anything
    /// else GitHub sends is ignored, which is what keeps this from breaking on
    /// a field being added.
    const RELEASE_JSON: &str = r#"{
      "tag_name": "v0.2.0",
      "name": "v0.2.0",
      "draft": false,
      "assets": [
        {"name": "sbx-v0.2.0-x86_64-unknown-linux-musl.tar.gz",
         "browser_download_url": "https://github.com/o/r/releases/download/v0.2.0/sbx-v0.2.0-x86_64-unknown-linux-musl.tar.gz",
         "size": 4},
        {"name": "SHA256SUMS",
         "browser_download_url": "https://github.com/o/r/releases/download/v0.2.0/SHA256SUMS"}
      ]
    }"#;

    #[test]
    fn a_release_is_read_as_its_tag_its_version_and_its_assets() {
        let r = parse_release(RELEASE_JSON).expect("parses");
        assert_eq!(r.tag, "v0.2.0");
        assert_eq!(r.version, "0.2.0", "the `v` is not part of the version");
        assert!(r.asset(SUMS).is_some());
        assert!(
            r.asset(&asset_name(&r.tag, "x86_64-unknown-linux-musl"))
                .is_some(),
            "the name this builds must be the name the release publishes"
        );
    }

    /// GitHub answers "no releases yet" with a 404 body rather than an empty
    /// list, and a repository can carry tags that are not versions. Neither may
    /// come back as an update.
    #[test]
    fn anything_that_is_not_a_version_is_not_a_release() {
        assert!(parse_release(r#"{"message":"Not Found"}"#).is_none());
        assert!(parse_release(r#"{"tag_name":"nightly","assets":[]}"#).is_none());
        assert!(parse_release("").is_none());
    }

    /// The comparison is [`crate::image::is_older`], so `0.1.9` must not count
    /// as newer than `0.1.10`, and a build ahead of the release must be left
    /// alone rather than "updated" backwards.
    #[test]
    fn only_a_newer_release_counts_as_an_update() {
        assert!(is_older("0.1.9", "0.1.10"));
        assert!(!is_older("0.2.0", "0.1.10"));
        assert!(!is_older("0.1.0", "0.1.0"));
    }

    #[test]
    fn checksums_are_read_whichever_way_sha256sum_wrote_them() {
        let sums = "\
aaaa  sbx-v0.2.0-x86_64-unknown-linux-musl.tar.gz
bbbb *sbx-v0.2.0-aarch64-unknown-linux-musl.tar.gz
";
        assert_eq!(
            expected_sha(sums, "sbx-v0.2.0-x86_64-unknown-linux-musl.tar.gz").as_deref(),
            Some("aaaa")
        );
        assert_eq!(
            expected_sha(sums, "sbx-v0.2.0-aarch64-unknown-linux-musl.tar.gz").as_deref(),
            Some("bbbb")
        );
        // An asset the file does not cover is not an asset that gets installed.
        assert_eq!(expected_sha(sums, "sbx-v0.2.0-something-else.tar.gz"), None);
    }

    #[test]
    fn releases_are_built_for_linux_only() {
        assert_eq!(
            target_for("linux", "x86_64"),
            Some("x86_64-unknown-linux-musl")
        );
        assert_eq!(
            target_for("linux", "aarch64"),
            Some("aarch64-unknown-linux-musl")
        );
        // The isolation is kernel-enforced, so there is nothing to ship here.
        assert_eq!(target_for("macos", "aarch64"), None);
        assert_eq!(target_for("linux", "riscv64"), None);
    }

    const INSTALL_SH: &str = include_str!("../../../install.sh");
    const RELEASE_WORKFLOW: &str = include_str!("../../../.github/workflows/release.yml");

    /// Three files have to agree about what a release asset is called: the
    /// workflow that publishes it, the script that installs it, and this. They
    /// are separately written and only ever wrong together at a distance, so
    /// the agreement is a test rather than a convention.
    #[test]
    fn the_installer_the_workflow_and_the_updater_agree_on_the_names() {
        for target in ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"] {
            assert!(
                RELEASE_WORKFLOW.contains(target),
                "the release workflow does not build {target}, which this would try to download"
            );
            assert!(
                INSTALL_SH.contains(target),
                "install.sh cannot install {target}"
            );
        }
        // `sbx-$tag-$target.tar.gz`, built by `asset_name` here and by string
        // interpolation there.
        assert!(
            INSTALL_SH.contains("sbx-${tag}-${target}.tar.gz"),
            "install.sh names assets differently from asset_name"
        );
        assert!(
            RELEASE_WORKFLOW.contains("sbx-${TAG}-${{ matrix.target }}.tar.gz"),
            "the release workflow names assets differently from asset_name"
        );
        assert_eq!(
            asset_name("v0.2.0", "x86_64-unknown-linux-musl"),
            "sbx-v0.2.0-x86_64-unknown-linux-musl.tar.gz"
        );
        assert!(
            INSTALL_SH.contains(SUMS) && RELEASE_WORKFLOW.contains(SUMS),
            "both must publish and read {SUMS}"
        );

        // ... and what is *inside* the archive, which is the half that used to
        // be nowhere: the server shipped only through `cargo install`.
        assert!(
            RELEASE_WORKFLOW.contains(&format!("--bin {CLIENT} --bin {SERVER}")),
            "the release workflow no longer builds both binaries"
        );
        assert!(
            RELEASE_WORKFLOW.contains(&format!(r#"-C "$bin" {CLIENT} {SERVER}"#)),
            "the release workflow no longer packs both binaries flat in the archive"
        );
        assert!(
            INSTALL_SH.contains(&format!("${{tmp}}/{SERVER}")),
            "install.sh no longer installs the server out of the archive"
        );
    }

    /// Everything points at one repository, and a fork that changes it has one
    /// place to change.
    #[test]
    fn every_file_points_at_the_same_repository() {
        assert!(
            INSTALL_SH.contains(REPO),
            "install.sh installs from elsewhere"
        );
    }

    /// Only the swap tests need a scratch directory, and they are unix-only.
    #[cfg(unix)]
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sbx-update-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The swap is the only step that touches a path outside a temporary
    /// directory, and the only one that cannot be retried if it goes wrong
    /// halfway. `/tmp` is very often a different filesystem from `~/.cargo/bin`,
    /// so the staging file has to be a sibling of the target rather than the
    /// download itself.
    #[test]
    #[cfg(unix)]
    fn the_new_binary_lands_on_the_old_one_from_another_filesystem() {
        let downloaded = scratch("swap-src");
        let installed = scratch("swap-dst");
        let fresh = downloaded.join("sbx");
        let at = installed.join("sbx");
        std::fs::write(&fresh, "new").unwrap();
        std::fs::write(&at, "old").unwrap();

        swap(&fresh, &at).expect("swaps");

        assert_eq!(std::fs::read_to_string(&at).unwrap(), "new");
        // Executable, or the update is an install that broke the tool.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&at).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "the replacement must be runnable");
        // Nothing left behind: a directory full of `.sbx-update-*` would be the
        // visible symptom of a swap that half-happened.
        let strays: Vec<_> = std::fs::read_dir(&installed)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".sbx-update"))
            .collect();
        assert!(strays.is_empty(), "left staging files behind");

        let _ = std::fs::remove_dir_all(&downloaded);
        let _ = std::fs::remove_dir_all(&installed);
    }

    /// The likely failure, and the one whose message has to carry its fix:
    /// `sbx` installed somewhere root owns.
    #[test]
    #[cfg(unix)]
    fn a_directory_that_cannot_be_written_says_what_to_do_about_it() {
        let downloaded = scratch("swap-ro-src");
        let installed = scratch("swap-ro-dst");
        let fresh = downloaded.join("sbx");
        std::fs::write(&fresh, "new").unwrap();
        let at = installed.join("sbx");
        std::fs::write(&at, "old").unwrap();

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Running the suite as root defeats the point of the test, and CI does
        // exactly that in a container.
        if std::fs::File::create(installed.join(".probe")).is_ok() {
            let _ = std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755));
            let _ = std::fs::remove_dir_all(&downloaded);
            let _ = std::fs::remove_dir_all(&installed);
            return;
        }

        let err = swap(&fresh, &at).expect_err("cannot write there");
        assert!(err.contains("fix:"), "no fix in: {err}");
        assert!(err.contains("sudo"), "the fix does not mention sudo: {err}");
        // The old binary is still the old binary, and still runnable.
        assert_eq!(std::fs::read_to_string(&at).unwrap(), "old");

        let _ = std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&downloaded);
        let _ = std::fs::remove_dir_all(&installed);
    }

    /// `sbx update` updates what is installed. A machine that drives only its
    /// own sandboxes has no `sbxd`, and an archive that happens to carry one
    /// is not a reason to give it a server it never asked to run.
    #[test]
    #[cfg(unix)]
    fn a_server_that_is_not_installed_is_not_installed_by_an_update() {
        let unpacked = scratch("no-server-src");
        let bin = scratch("no-server-dst");
        std::fs::write(unpacked.join(SERVER), "fresh").unwrap();
        std::fs::write(bin.join(CLIENT), "old sbx").unwrap();

        assert_eq!(swap_server(&unpacked, &bin.join(CLIENT)), Ok(None));
        assert!(
            !bin.join(SERVER).exists(),
            "an update must not conjure up a server binary"
        );

        let _ = std::fs::remove_dir_all(&unpacked);
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// When it *is* installed, it moves with the client -- beside it, because
    /// that is where `install.sh` put it.
    #[test]
    #[cfg(unix)]
    fn an_installed_server_is_replaced_beside_the_client() {
        let unpacked = scratch("with-server-src");
        let bin = scratch("with-server-dst");
        std::fs::write(unpacked.join(SERVER), "fresh sbxd").unwrap();
        std::fs::write(bin.join(CLIENT), "old sbx").unwrap();
        std::fs::write(bin.join(SERVER), "old sbxd").unwrap();

        let swapped = swap_server(&unpacked, &bin.join(CLIENT)).expect("swaps");
        assert_eq!(swapped.as_deref(), Some(bin.join(SERVER).as_path()));
        assert_eq!(
            std::fs::read_to_string(bin.join(SERVER)).unwrap(),
            "fresh sbxd"
        );
        // Nothing staged is left lying about next to the real thing.
        let stray: Vec<_> = std::fs::read_dir(&bin)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(stray.is_empty(), "left staging files behind: {stray:?}");

        let _ = std::fs::remove_dir_all(&unpacked);
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// Every release up to v0.3.0 packs `sbx` alone, and `sbx update --tag`
    /// may still be pointed at one. That is a client update, not a failure.
    #[test]
    #[cfg(unix)]
    fn an_archive_with_no_server_in_it_is_not_an_error() {
        let unpacked = scratch("old-archive-src");
        let bin = scratch("old-archive-dst");
        std::fs::write(bin.join(CLIENT), "old sbx").unwrap();
        std::fs::write(bin.join(SERVER), "installed sbxd").unwrap();

        assert_eq!(swap_server(&unpacked, &bin.join(CLIENT)), Ok(None));
        assert_eq!(
            std::fs::read_to_string(bin.join(SERVER)).unwrap(),
            "installed sbxd",
            "an archive without a server must leave the installed one alone"
        );

        let _ = std::fs::remove_dir_all(&unpacked);
        let _ = std::fs::remove_dir_all(&bin);
    }
}
