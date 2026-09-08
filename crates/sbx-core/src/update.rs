//! Updating `sbxd` itself.
//!
//! The tool that installs it is `install.sh`, which needs no checkout and no
//! Rust toolchain: it fetches the newest release, checks it against the
//! published SHA-256, and drops the binary into a directory on `PATH`. This
//! module is the same three steps performed by the binary already installed, so
//! updating is `sbxd update` rather than remembering the curl line.
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
//! anyone remembering.
//!
//! **The asset was renamed in v0.4.0**, from `sbx-<tag>-<target>.tar.gz` to
//! `sbxd-<tag>-<target>.tar.gz`, when `sbx` was folded into `sbxd`. Nothing
//! older can follow that: `sbx update` replaces the binary it is running, and
//! from v0.4.0 there is no `sbx` to replace it with. It fails saying the
//! release has no asset by the name it wants, which is true and is the least
//! confusing thing it could say.
//!
//! **The way across is one `install.sh`, and there is no update path that
//! crosses it.** Not even from v0.3.1: the `sbxd` that release installs is the
//! server alone, since this module did not move into it until v0.4.0, so it
//! cannot fetch its own successor. v0.3.1 is worth having for a narrower
//! reason -- it is the last release `sbx update` can reach, so nobody is
//! stranded on the v0.3.0 that no installed copy could update to.

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
    format!("{BIN}-{tag}-{target}.tar.gz")
}

/// The checksum file covering every asset in a release.
const SUMS: &str = "SHA256SUMS";

/// The one binary a release carries, and the one this replaces.
const BIN: &str = "sbxd";

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
            format!("no release tagged `{t}`, or github could not be reached\n     fix: `sbxd update` takes the newest; see https://github.com/{REPO}/releases")
        })?,
        None => latest().ok_or_else(|| {
            format!("could not read the release list -- there may be none published yet\n     fix: build from source with `cargo install --git https://github.com/{REPO} sbxd --locked`")
        })?,
    };

    if release.version == current() && !force {
        return Ok(Outcome::NoChange {
            version: release.version,
        });
    }

    let at = std::env::current_exe().map_err(|e| format!("cannot find the running binary: {e}"))?;
    let scratch = Scratch::new()?;
    let fresh = fetch_verified(scratch.path(), &release)?;
    swap(&fresh, &at)?;
    Ok(Outcome::Updated {
        from: current().to_string(),
        to: release.version.clone(),
        at,
    })
}

// -------------------------------------------------------------- auto-updating

/// How often [`serve`] asks whether there is a newer release.
///
/// [`serve`]: https://docs.rs/sbxd
pub const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Where a downloaded release waits until something restarts.
///
/// Beside the binary rather than in `/tmp`: [`swap`] renames it into place, and
/// a rename across filesystems fails with `EXDEV`. A dotfile, so it does not
/// appear in a `PATH` completion as a second command.
fn staged_at(at: &Path) -> PathBuf {
    at.with_file_name(format!(".{BIN}-staged"))
}

/// Fetch the newest release and leave it beside the running binary, replacing
/// nothing.
///
/// This is the half of an automatic update that is safe to do while the server
/// is working: it costs one API call when there is nothing new, and when there
/// is, the download and the checksum and the version check all happen against a
/// file nothing is running. [`apply_staged`] is the other half, and it happens
/// at a process start.
///
/// `Ok(None)` means there was nothing newer, which is the ordinary answer and
/// not a failure. A network that is not answering is `Err`, because a caller
/// logging "could not check" is telling the truth and a caller told "up to
/// date" would not be.
pub fn stage() -> Result<Option<String>, String> {
    let release = latest().ok_or("could not read the release list")?;
    if !is_older(current(), &release.version) {
        return Ok(None);
    }

    let at = std::env::current_exe().map_err(|e| format!("cannot find the running binary: {e}"))?;
    let staged = staged_at(&at);
    // Already waiting, and for this version: downloading it again every six
    // hours until somebody restarts would be a lot of bandwidth to prove a
    // point.
    if version_of(&staged).as_deref() == Some(release.version.as_str()) {
        return Ok(None);
    }

    let scratch = Scratch::new()?;
    let fresh = fetch_verified(scratch.path(), &release)?;
    // Through the same copy-then-rename as an install, so a torn write cannot
    // leave a half-file that the next start would try to run.
    swap(&fresh, &staged)?;
    Ok(Some(release.version))
}

/// Move a staged release into place, if one is waiting.
///
/// Called at the top of `main`, before anything has happened, so the swap
/// cannot land halfway through a command. Returns where the new binary is, and
/// the caller is expected to `exec` into it -- otherwise this process carries
/// on as the version it already was, and the update would need a second
/// restart to take effect.
///
/// **A running server is not disturbed by this.** Linux keeps an executing
/// binary on its old inode through a rename, so an `sbxd` serving sessions goes
/// on serving them as the version it started as, however many times another
/// invocation swaps the file underneath it. The new one is what the next start
/// gets, which is the whole point of staging rather than installing.
pub fn apply_staged() -> Option<PathBuf> {
    let at = std::env::current_exe().ok()?;
    let staged = staged_at(&at);
    if !staged.is_file() {
        return None;
    }

    // What the staged file says it is, asked of the file itself rather than
    // remembered from when it was downloaded: a note beside it could disagree
    // with it, and the file is the thing that will run.
    let staged_version = version_of(&staged);
    let newer = staged_version
        .as_deref()
        .is_some_and(|v| is_older(current(), v));
    if !newer {
        // Stale, damaged, or a downgrade -- and in every one of those cases the
        // right move is to stop carrying it around. A staged binary that is not
        // newer will never become newer.
        let _ = std::fs::remove_file(&staged);
        return None;
    }

    if let Err(e) = swap(&staged, &at) {
        // Not fatal, and deliberately not loud: the binary that is running is
        // fine, and a server that refuses to start because it could not update
        // itself would be a worse failure than the one it is reporting.
        eprintln!("{BIN}: could not apply the staged update: {e}");
        return None;
    }
    let _ = std::fs::remove_file(&staged);
    Some(at)
}

/// What a binary on disk says its version is, or `None` if it will not run.
fn version_of(bin: &Path) -> Option<String> {
    if !bin.is_file() {
        return None;
    }
    let out = Command::new(bin).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().nth(1).map(str::to_string)
}

// ------------------------------------------------------------------ downloads

/// A temporary directory that removes itself, so every path out of a download
/// cleans up -- including the failures, of which there are several.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!("{BIN}-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Scratch(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Download a release, check it against the published sum, unpack it, and
/// confirm the binary inside is the version the release claims.
///
/// Everything up to the moment of replacement, and nothing after it: the caller
/// decides whether the result goes over the running binary ([`install`]) or
/// beside it ([`stage`]). Shared so an automatic update cannot end up with
/// weaker checks than a manual one -- which is the failure this arrangement
/// exists to prevent.
fn fetch_verified(dir: &Path, release: &Release) -> Result<PathBuf, String> {
    let target = target().ok_or_else(|| {
        format!(
            "no release is built for {} {}\n     fix: build from source with `cargo install --git https://github.com/{REPO} sbxd --locked`",
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

    let tarball = dir.join(&name);
    download(&asset.url, &tarball)?;
    let sums_path = dir.join(SUMS);
    download(&sums.url, &sums_path)?;

    let published =
        std::fs::read_to_string(&sums_path).map_err(|e| format!("{}: {e}", sums_path.display()))?;
    let expected =
        expected_sha(&published, &name).ok_or_else(|| format!("{SUMS} does not cover {name}"))?;
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
    let fresh = dir.join(BIN);
    if !fresh.is_file() {
        return Err(format!("{name} does not contain an `{BIN}` binary"));
    }
    make_executable(&fresh)?;

    // Ask the downloaded binary what it is before trusting it with the path the
    // running one occupies. A release whose asset was built from the wrong
    // commit answers the wrong version here, which is cheaper to find out now
    // than after the swap -- and is exactly what went wrong in v0.3.0.
    match version_of(&fresh).as_deref() {
        Some(v) if v == release.version => Ok(fresh),
        Some(v) => Err(format!(
            "the downloaded binary reports {v}, not {}",
            release.version
        )),
        None => Err("the downloaded binary does not run".into()),
    }
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
    let staged = dir.join(format!(".{BIN}-update-{}", std::process::id()));
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
        {"name": "sbxd-v0.2.0-x86_64-unknown-linux-musl.tar.gz",
         "browser_download_url": "https://github.com/o/r/releases/download/v0.2.0/sbxd-v0.2.0-x86_64-unknown-linux-musl.tar.gz",
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
aaaa  sbxd-v0.2.0-x86_64-unknown-linux-musl.tar.gz
bbbb *sbxd-v0.2.0-aarch64-unknown-linux-musl.tar.gz
";
        assert_eq!(
            expected_sha(sums, "sbxd-v0.2.0-x86_64-unknown-linux-musl.tar.gz").as_deref(),
            Some("aaaa")
        );
        assert_eq!(
            expected_sha(sums, "sbxd-v0.2.0-aarch64-unknown-linux-musl.tar.gz").as_deref(),
            Some("bbbb")
        );
        // An asset the file does not cover is not an asset that gets installed.
        assert_eq!(
            expected_sha(sums, "sbxd-v0.2.0-something-else.tar.gz"),
            None
        );
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
            INSTALL_SH.contains("sbxd-${tag}-${target}.tar.gz"),
            "install.sh names assets differently from asset_name"
        );
        assert!(
            RELEASE_WORKFLOW.contains("sbxd-${TAG}-${{ matrix.target }}.tar.gz"),
            "the release workflow names assets differently from asset_name"
        );
        assert_eq!(
            asset_name("v0.2.0", "x86_64-unknown-linux-musl"),
            "sbxd-v0.2.0-x86_64-unknown-linux-musl.tar.gz"
        );
        assert!(
            INSTALL_SH.contains(SUMS) && RELEASE_WORKFLOW.contains(SUMS),
            "both must publish and read {SUMS}"
        );

        // ... and what is *inside* it, which is the half that used to be
        // nowhere: before v0.3.1 the server shipped only through `cargo
        // install`, and from v0.4.0 it is the only thing that ships at all.
        assert!(
            RELEASE_WORKFLOW.contains(&format!("--bin {BIN}")),
            "the release workflow no longer builds {BIN}"
        );
        assert!(
            RELEASE_WORKFLOW.contains(&format!(r#"-C "$bin" {BIN}"#)),
            "the release workflow no longer packs {BIN} flat in the archive"
        );
        assert!(
            INSTALL_SH.contains(&format!("${{tmp}}/{BIN}")),
            "install.sh no longer installs {BIN} out of the archive"
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
        let fresh = downloaded.join(BIN);
        let at = installed.join(BIN);
        std::fs::write(&fresh, "new").unwrap();
        std::fs::write(&at, "old").unwrap();

        swap(&fresh, &at).expect("swaps");

        assert_eq!(std::fs::read_to_string(&at).unwrap(), "new");
        // Executable, or the update is an install that broke the tool.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&at).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "the replacement must be runnable");
        // Nothing left behind: a directory full of `.sbxd-update-*` would be
        // the visible symptom of a swap that half-happened.
        let strays: Vec<_> = std::fs::read_dir(&installed)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".sbxd-update"))
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

    /// A stand-in for a released binary: something that runs and answers
    /// `--version`, which is all [`version_of`] and [`apply_staged`] ask of one.
    #[cfg(unix)]
    fn fake_binary(at: &Path, version: &str) {
        std::fs::write(at, format!("#!/bin/sh\necho '{BIN} {version}'\n")).unwrap();
        make_executable(at).unwrap();
    }

    /// The ordinary case, and the cheap one: nothing downloaded, nothing to do,
    /// and this runs at the top of every single command.
    #[test]
    #[cfg(unix)]
    fn nothing_staged_is_nothing_to_apply() {
        let bin = scratch("apply-none");
        fake_binary(&bin.join(BIN), "0.4.0");
        assert!(!staged_at(&bin.join(BIN)).exists());
        // `apply_staged` reads `current_exe`, so the unit under test here is
        // the decision, driven directly.
        assert_eq!(version_of(&staged_at(&bin.join(BIN))), None);
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// What a staged file is asked, and what it answers when it is not a
    /// binary at all -- a truncated download, or a half-written swap.
    #[test]
    #[cfg(unix)]
    fn a_staged_file_that_will_not_run_has_no_version() {
        let bin = scratch("apply-junk");
        let staged = bin.join("staged");
        std::fs::write(&staged, "not a binary").unwrap();
        make_executable(&staged).unwrap();
        assert_eq!(version_of(&staged), None, "junk must not report a version");
        let _ = std::fs::remove_dir_all(&bin);
    }

    #[test]
    #[cfg(unix)]
    fn a_staged_binary_reports_the_version_it_will_run_as() {
        let bin = scratch("apply-version");
        let staged = bin.join("staged");
        fake_binary(&staged, "0.5.0");
        assert_eq!(version_of(&staged).as_deref(), Some("0.5.0"));
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// The staged path is beside the binary and not in `/tmp`, because the swap
    /// is a rename and a rename across filesystems fails with `EXDEV`. A
    /// dotfile, so it is not a second command in a `PATH` completion.
    #[test]
    fn a_staged_release_waits_beside_the_binary_it_will_replace() {
        let at = Path::new("/home/someone/.local/bin").join(BIN);
        let staged = staged_at(&at);
        assert_eq!(staged.parent(), at.parent(), "a rename must not cross /tmp");
        assert!(
            staged
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with('.'),
            "the staged file must not look like a command"
        );
        assert_ne!(staged, at);
    }

    /// Only forwards. A staged file that is not newer than what is running --
    /// stale after a manual `sbxd update`, or a downgrade -- must not be
    /// applied, and must not be kept either: it will never become newer.
    #[test]
    fn only_a_newer_staged_release_is_worth_applying() {
        assert!(is_older("0.4.0", "0.4.1"), "newer is applied");
        assert!(!is_older("0.4.1", "0.4.0"), "a downgrade is not");
        assert!(!is_older("0.4.0", "0.4.0"), "the same version is not");
    }
}
