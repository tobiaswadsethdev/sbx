//! Agent skills, carried from the host into a session.
//!
//! A skill is a directory with a `SKILL.md` in it, and the agent reads the ones
//! under `~/.claude/skills`. A sandbox has its own `HOME` and its own
//! filesystem, so a fresh one has none: the skills you have spent months
//! sharpening are the one part of your setup that does not follow you in.
//!
//! **A symlink cannot cross the boundary, and neither can a bind mount.** The
//! isolation is the product; a sandbox that could read `~/.claude` could read
//! everything else in `$HOME` too. So this is a copy, taken while the sandbox is
//! seeded. What the config file holds is the *pointer* -- edit the original and
//! the next session gets the edit, which is the part of a symlink that is
//! actually wanted here. A session already running keeps what it was created
//! with, and its record says so.
//!
//! Copied with `tar` on the host and `tar` in the sandbox rather than file by
//! file, because a skill is a directory: `SKILL.md` beside its scripts,
//! references and templates, and a passthrough that quietly moved only the
//! markdown would be worse than none. Symlinks are followed (`-h`), so a skill
//! that is itself a link to somewhere else arrives as its contents.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Where the agent looks for skills inside the sandbox. `HOME` is `/sandbox`.
pub const SANDBOX_SKILLS_DIR: &str = "/sandbox/.claude/skills";

/// The file that makes a directory a skill.
const MANIFEST: &str = "SKILL.md";

/// The most base64 one skill may weigh, after compression.
///
/// The payload rides inside the seeder script, which is itself written into the
/// sandbox through an `exec` argument, so this is bounded by what a command line
/// can hold rather than by anything in the gateway. 256 KiB of base64 is ~190KiB
/// of gzip and far more prose than any skill has; a skill above it is one that
/// has a virtualenv or a video in it by accident, and saying so is more useful
/// than a create that fails on `argument list too long`.
const MAX_PAYLOAD: usize = 256 * 1024;

/// One skill, as the config file points at it and as the sandbox records it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// The directory name, which is what the agent calls the skill.
    pub name: String,
    /// Where it was read from on the host. Kept in the record so a session can
    /// say where its copy came from, months later.
    pub source: PathBuf,
}

impl Skill {
    /// Resolve a config entry.
    ///
    /// A bare name is one of your own skills, under `~/.claude/skills`, because
    /// that is where the agent keeps them and typing the path every time would
    /// be noise. Anything with a separator in it is a path, so a skill living in
    /// a repository -- or in a plugin's cache -- can be pointed at where it
    /// actually is.
    pub fn parse(entry: &str) -> Result<Self, Error> {
        let entry = entry.trim();
        if entry.is_empty() {
            return Err(Error::Empty);
        }

        let path = if entry.contains('/') || entry.starts_with('~') {
            expand_tilde(Path::new(entry))
        } else {
            host_skills_dir().join(entry)
        };

        // Trailing slashes are how a shell completes a directory, and they would
        // otherwise leave the name empty.
        let name = path
            .components()
            .next_back()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .filter(|n| !n.is_empty() && n != "." && n != "..")
            .ok_or_else(|| Error::NoName(entry.to_string()))?;

        Ok(Skill { name, source: path })
    }

    /// What is wrong with this skill on disk, if anything.
    ///
    /// Separate from [`Self::parse`] so the config file can be read without
    /// touching the filesystem, and so `doctor` and the create path can ask the
    /// same question and get the same words.
    pub fn problem(&self) -> Option<String> {
        if !self.source.exists() {
            return Some(format!("{} does not exist", self.source.display()));
        }
        if !self.source.is_dir() {
            return Some(format!(
                "{} is a file; a skill is a directory with a {MANIFEST} in it",
                self.source.display()
            ));
        }
        if !self.source.join(MANIFEST).is_file() {
            return Some(format!(
                "{} has no {MANIFEST}, so the agent would not load it",
                self.source.display()
            ));
        }
        None
    }
}

/// Where this host keeps its skills: `$CLAUDE_CONFIG_DIR/skills`, else
/// `~/.claude/skills`. The same two places the agent itself looks.
pub fn host_skills_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(dir).join("skills");
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".claude").join("skills")
}

/// The seeder's skills step: unpack every skill into the sandbox.
///
/// Returns the script and whatever could not be packed, rather than failing on
/// the first bad one. A missing skill is a warning at create time and a session
/// that is merely missing a skill; refusing to create it would be a worse trade,
/// and `doctor` says which one is wrong before you ever get here.
pub fn pack(skills: &[Skill]) -> (String, Vec<String>) {
    let mut script = String::new();
    let mut warnings = Vec::new();

    for skill in skills {
        if let Some(problem) = skill.problem() {
            warnings.push(format!("skill `{}` was not copied: {problem}", skill.name));
            continue;
        }
        match payload(&skill.source) {
            Ok(b64) => {
                // Removed and re-extracted rather than extracted over: tar
                // overwrites what it carries and leaves everything else, so a
                // file deleted from the skill since the last seed would live on
                // in a re-seeded sandbox.
                script.push_str(&format!(
                    "rm -rf {dest}\n\
                     printf '%s' {b64} | base64 -d | tar -xzf - -C {dir}\n",
                    dest = crate::seed::sh_quote(
                        &Path::new(SANDBOX_SKILLS_DIR)
                            .join(&skill.name)
                            .to_string_lossy()
                    ),
                    b64 = crate::seed::sh_quote(&b64),
                    dir = crate::seed::sh_quote(SANDBOX_SKILLS_DIR),
                ));
            }
            Err(e) => warnings.push(format!("skill `{}` was not copied: {e}", skill.name)),
        }
    }

    if !script.is_empty() {
        script.insert_str(
            0,
            &format!("mkdir -p {}\n", crate::seed::sh_quote(SANDBOX_SKILLS_DIR)),
        );
    }
    (script, warnings)
}

/// One skill directory as base64 of a gzipped tar.
///
/// `tar` on the host rather than a walk in here: it is the tool for the job, it
/// is already a dependency of nothing (every system running Docker has it), and
/// permissions, nesting and empty directories come out the other end unchanged.
fn payload(source: &Path) -> Result<String, Error> {
    let parent = source.parent().ok_or(Error::NoParent)?;
    let name = source
        .file_name()
        .ok_or_else(|| Error::NoName(source.display().to_string()))?;

    let out = Command::new("tar")
        .arg("-czhf")
        .arg("-")
        .arg("-C")
        .arg(parent)
        .arg(name)
        .output()
        .map_err(|e| Error::Tar(e.to_string()))?;
    if !out.status.success() {
        return Err(Error::Tar(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }

    let b64 = base64(&out.stdout);
    if b64.len() > MAX_PAYLOAD {
        return Err(Error::TooBig {
            kib: b64.len() / 1024,
            max_kib: MAX_PAYLOAD / 1024,
        });
    }
    Ok(b64)
}

/// Standard base64, padded. Written out rather than pulled in: it is fifteen
/// lines, and the alternative is a dependency for one call.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let idx = [
            (n >> 18) & 63,
            (n >> 12) & 63,
            (n >> 6) & 63,
            n & 63,
            // The last one or two characters are padding when the chunk was
            // short, which is the whole of what makes this not a loop over
            // four indices.
        ];
        for (i, index) in idx.iter().enumerate() {
            if i > chunk.len() {
                out.push('=');
            } else {
                out.push(char::from(ALPHABET[*index as usize]));
            }
        }
    }
    out
}

/// `~` and `~/...`, as [`crate::config`] expands them for `repo_roots`.
fn expand_tilde(path: &Path) -> PathBuf {
    let Some(rest) = path.to_str().and_then(|s| s.strip_prefix('~')) else {
        return path.to_path_buf();
    };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return path.to_path_buf();
    };
    match rest.strip_prefix('/') {
        Some(tail) => home.join(tail),
        None if rest.is_empty() => home,
        None => path.to_path_buf(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("is empty")]
    Empty,
    #[error("`{0}` names no directory")]
    NoName(String),
    #[error("has no parent directory")]
    NoParent,
    #[error("could not be packed: {0}")]
    Tar(String),
    #[error("is {kib}KiB packed, over the {max_kib}KiB a skill may weigh")]
    TooBig { kib: usize, max_kib: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A skill directory in a temp dir, returned with its path.
    fn a_skill(name: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("sbx-skills-test-{}-{name}", std::process::id()));
        let dir = root.join(name);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&dir).unwrap();
        for (file, content) in files {
            let path = dir.join(file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, content).unwrap();
        }
        (root, dir)
    }

    #[test]
    fn base64_matches_the_reference_vectors() {
        // RFC 4648 test vectors: the padding is the part that is easy to get
        // wrong, and the part `base64 -d` in the sandbox will not forgive.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0u8, 255, 128]), "AP+A");
    }

    /// The encoder is only useful if the sandbox's `base64 -d` accepts it, so
    /// the assertion is a real decoder rather than another table.
    #[test]
    fn base64_round_trips_through_a_real_decoder() {
        let bytes: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let encoded = base64(&bytes);
        let out = Command::new("base64")
            .arg("-d")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as _;
                child
                    .stdin
                    .take()
                    .expect("stdin")
                    .write_all(encoded.as_bytes())?;
                child.wait_with_output()
            })
            .expect("base64");
        assert!(out.status.success());
        assert_eq!(out.stdout, bytes);
    }

    #[test]
    fn a_bare_name_is_one_of_your_own_skills() {
        let s = Skill::parse("ship-pr").unwrap();
        assert_eq!(s.name, "ship-pr");
        assert_eq!(s.source, host_skills_dir().join("ship-pr"));
    }

    #[test]
    fn a_path_is_taken_as_one_and_names_the_skill_after_its_directory() {
        let s = Skill::parse("/srv/repo/.claude/skills/deploy").unwrap();
        assert_eq!(s.name, "deploy");
        assert_eq!(s.source, PathBuf::from("/srv/repo/.claude/skills/deploy"));

        // A trailing slash is how a shell completes a directory.
        assert_eq!(Skill::parse("/srv/skills/deploy/").unwrap().name, "deploy");
    }

    #[test]
    fn tilde_is_expanded() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let s = Skill::parse("~/work/skills/audit").unwrap();
        assert_eq!(s.source, home.join("work/skills/audit"));
    }

    #[test]
    fn a_directory_without_a_manifest_is_not_a_skill() {
        let (root, dir) = a_skill("nomanifest", &[("notes.md", "hi")]);
        let s = Skill {
            name: "nomanifest".into(),
            source: dir,
        };
        let problem = s.problem().expect("a problem");
        assert!(problem.contains("SKILL.md"), "{problem}");

        let missing = Skill {
            name: "gone".into(),
            source: root.join("gone"),
        };
        assert!(missing.problem().unwrap().contains("does not exist"));
        let _ = fs::remove_dir_all(root);
    }

    /// The whole point of packing a directory rather than a file: a skill is
    /// its manifest *and* whatever it carries.
    #[test]
    fn packing_carries_the_whole_directory() {
        let (root, dir) = a_skill(
            "ship",
            &[
                ("SKILL.md", "---\nname: ship\n---\n"),
                ("scripts/run.sh", "#!/bin/sh\necho hi\n"),
                ("references/notes.md", "# notes"),
            ],
        );
        let skill = Skill {
            name: "ship".into(),
            source: dir,
        };
        let (script, warnings) = pack(std::slice::from_ref(&skill));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(script.starts_with("mkdir -p '/sandbox/.claude/skills'"));
        assert!(script.contains("rm -rf '/sandbox/.claude/skills/ship'"));
        assert!(script.contains("| base64 -d | tar -xzf - -C '/sandbox/.claude/skills'"));

        // Unpack what the script carries and check every file survived. The
        // payload is the fourth field of `printf '%s' '<b64>' | ...`: base64
        // contains `/` and `+` but never a quote, so splitting on quotes is
        // exact where matching on the shape of the text is not.
        let line = script
            .lines()
            .find(|l| l.starts_with("printf "))
            .expect("a printf line");
        let b64 = line.split('\'').nth(3).expect("the payload");
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "printf '%s' {} | base64 -d | tar -tzf -",
                crate::seed::sh_quote(b64)
            ))
            .output()
            .expect("sh");
        let listing = String::from_utf8_lossy(&out.stdout);
        for wanted in [
            "ship/SKILL.md",
            "ship/scripts/run.sh",
            "ship/references/notes.md",
        ] {
            assert!(listing.contains(wanted), "{wanted} missing from {listing}");
        }
        let _ = fs::remove_dir_all(root);
    }

    /// A skill that cannot be copied costs the skill, not the session.
    #[test]
    fn a_missing_skill_is_a_warning_and_not_a_script() {
        let skill = Skill {
            name: "nope".into(),
            source: PathBuf::from("/nonexistent/skills/nope"),
        };
        let (script, warnings) = pack(&[skill]);
        assert!(script.is_empty(), "{script}");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("nope"), "{warnings:?}");
    }

    #[test]
    fn nothing_configured_is_no_step() {
        let (script, warnings) = pack(&[]);
        assert!(script.is_empty());
        assert!(warnings.is_empty());
    }
}
