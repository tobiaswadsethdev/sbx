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
//!
//! ## Two hosts, and a library between them
//!
//! "The host" used to mean one machine. With a server it means two: the sessions
//! run where `sbxd` is, and the skills live on the machine with the window on
//! it. A path in the server's config file cannot reach them.
//!
//! So the server keeps a **library** at `$XDG_DATA_HOME/sbx/skills`, filled from
//! either side -- paths in its own config file, exactly as before, and uploads
//! pushed by a client from its own `~/.claude/skills`. A session is given both.
//!
//! The pointer-not-copy property survives the extra hop, because the client
//! re-uploads before every create: editing a skill on your laptop still means
//! the next session gets the edit, which is the whole reason the config file
//! holds a path rather than a copy. What a session records is still exactly what
//! it was handed.

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

/// Where the server keeps the skills a client has uploaded.
///
/// `$XDG_DATA_HOME/sbx/skills`, beside the worktrees: data rather than state,
/// because it is the copy of something whose original is on another machine and
/// a server that lost it would hand every new session fewer skills without
/// saying so. Deliberately *not* the server user's own `~/.claude/skills`,
/// which is theirs -- writing an uploaded skill in there would change what the
/// agent they run themselves knows how to do.
pub fn library_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sbx")
        .join("skills")
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
///
/// Public because it is also what a client uploads: the same bytes, sent to the
/// server instead of into a seeder script.
pub fn payload(source: &Path) -> Result<String, Error> {
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

/// One skill on its way from a client to the server's library.
///
/// The payload is the same base64 gzipped tar the seeder carries, so there is
/// one packing routine and one unpacking shape. `origin` is where it came from
/// on the machine that sent it -- kept because a session's record says where its
/// skills came from, and "the library" would be a worse answer than
/// `/home/you/.claude/skills/ship-pr`.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "SkillUpload"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upload {
    pub name: String,
    pub origin: String,
    /// base64 of a gzipped tar whose single top-level entry is `name`.
    pub tar: String,
}

/// One skill the library holds, as a screen shows it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "StoredSkill"))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    pub name: String,
    /// Where it came from on the machine that uploaded it.
    pub origin: String,
    /// Epoch seconds of the last upload. A skill nobody has pushed for months
    /// is the one to suspect when an agent has forgotten how to do something.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub uploaded_at: u64,
}

/// What the library records about each skill in it, keyed by name.
///
/// A file beside the skills rather than a marker inside each one: everything
/// inside a skill directory is copied into the sandbox, and a `.sbx-origin`
/// landing in the agent's skill folder would be a file we put in someone's
/// prompt.
const ORIGINS: &str = ".origins.json";

/// The skills on *this* machine, for a client about to upload them.
///
/// Read from where the agent itself looks, so what a session gets is what you
/// have -- no list to maintain, and nothing to remember after adding a skill.
/// A directory with no `SKILL.md` is skipped rather than reported: the agent
/// ignores it too, and half the time it is an editor's leftovers.
pub fn local() -> Vec<Skill> {
    local_in(&host_skills_dir())
}

pub fn local_in(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Skill> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(MANIFEST).is_file())
        .filter_map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .filter(|n| !n.starts_with('.'))
                .map(|name| Skill {
                    name: name.to_string(),
                    source: p.clone(),
                })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Everything in the library, sorted by name.
pub fn library() -> Vec<Stored> {
    library_at(&library_dir())
}

pub fn library_at(dir: &Path) -> Vec<Stored> {
    let origins: std::collections::BTreeMap<String, Stored> =
        std::fs::read_to_string(dir.join(ORIGINS))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    // The directories are the truth and the file is decoration: a skill whose
    // record was lost is still a skill the agent will load, and saying nothing
    // about it would be a library that hides half of itself.
    let mut out: Vec<Stored> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| dir.join(name).join(MANIFEST).is_file())
        .map(|name| {
            origins.get(&name).cloned().unwrap_or_else(|| Stored {
                origin: "(unknown)".into(),
                uploaded_at: 0,
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The library as [`Skill`]s, which is what a session is given.
pub fn library_skills() -> Vec<Skill> {
    let dir = library_dir();
    library_at(&dir)
        .into_iter()
        .map(|s| Skill {
            source: dir.join(&s.name),
            name: s.name,
        })
        .collect()
}

/// Take one upload into the library, replacing whatever was there under that
/// name.
///
/// **Unpacked into a temporary directory and inspected before it is anywhere
/// that matters.** A tar arrives from a client, and a client is a program on
/// another machine: an archive whose members climb out with `..`, or carry a
/// second skill, or carry no `SKILL.md` at all, are all things to find out about
/// before they are in the directory every future session copies from. GNU tar
/// skips `..` members itself, which is a good default and not a guarantee this
/// module is willing to inherit.
pub fn install(dir: &Path, upload: &Upload) -> Result<Stored, String> {
    let name = valid_name(&upload.name)?;
    if upload.tar.len() > MAX_PAYLOAD {
        return Err(format!(
            "`{name}` is {}KiB packed, over the {}KiB a skill may weigh",
            upload.tar.len() / 1024,
            MAX_PAYLOAD / 1024
        ));
    }

    let staging = dir.join(format!(".incoming-{name}"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("could not unpack `{name}`: {e}"))?;

    let unpacked = unpack(&staging, &upload.tar).and_then(|()| {
        let entries: Vec<PathBuf> = std::fs::read_dir(&staging)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .collect();
        match entries.as_slice() {
            [only] if only.file_name().and_then(|n| n.to_str()) == Some(name.as_str()) => {
                if only.join(MANIFEST).is_file() {
                    Ok(only.clone())
                } else {
                    Err(format!("`{name}` has no {MANIFEST} in it"))
                }
            }
            [only] => Err(format!(
                "`{name}` unpacked as `{}`, which is not what it says it is",
                only.file_name().unwrap_or_default().to_string_lossy()
            )),
            other => Err(format!(
                "`{name}` unpacked as {} top-level entries; a skill is one directory",
                other.len()
            )),
        }
    });

    let staged = match unpacked {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };

    // Removed and replaced rather than unpacked over, for the reason the
    // seeder does the same: a file deleted from the skill since the last upload
    // would otherwise live on in the library for ever.
    let dest = dir.join(&name);
    let _ = std::fs::remove_dir_all(&dest);
    let moved =
        std::fs::rename(&staged, &dest).map_err(|e| format!("could not store `{name}`: {e}"));
    let _ = std::fs::remove_dir_all(&staging);
    moved?;

    let stored = Stored {
        name,
        origin: upload.origin.trim().to_string(),
        uploaded_at: crate::session::now_epoch(),
    };
    record_origin(dir, &stored)?;
    Ok(stored)
}

/// Drop one from the library. The client's own copy is untouched -- this is a
/// cache of somebody else's directory.
pub fn forget(dir: &Path, name: &str) -> Result<(), String> {
    let name = valid_name(name)?;
    std::fs::remove_dir_all(dir.join(&name))
        .or_else(|e| match e.kind() {
            // Already gone is the end state that was asked for.
            std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(format!("could not remove `{name}`: {e}")),
        })
        .and_then(|()| {
            let mut origins = read_origins(dir);
            origins.remove(&name);
            write_origins(dir, &origins)
        })
}

fn record_origin(dir: &Path, stored: &Stored) -> Result<(), String> {
    let mut origins = read_origins(dir);
    origins.insert(stored.name.clone(), stored.clone());
    write_origins(dir, &origins)
}

fn read_origins(dir: &Path) -> std::collections::BTreeMap<String, Stored> {
    std::fs::read_to_string(dir.join(ORIGINS))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_origins(
    dir: &Path,
    origins: &std::collections::BTreeMap<String, Stored>,
) -> Result<(), String> {
    let json = serde_json::to_string_pretty(origins).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(ORIGINS), json)
        .map_err(|e| format!("could not record where the skills came from: {e}"))
}

/// A directory name, and nothing that could be a path.
///
/// The name decides a directory under the library and the destination inside
/// every sandbox, and it arrives from a client. A separator or a `..` in it is
/// the whole class of mistake worth refusing by shape rather than by sanitising.
fn valid_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a skill needs a name".into());
    }
    if name == "." || name == ".." || name.starts_with('.') {
        return Err(format!("`{name}` is not a skill name"));
    }
    if let Some(c) = name
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.'))
    {
        return Err(format!(
            "`{name}` is not a skill name; `{c}` is not allowed in one"
        ));
    }
    Ok(name.to_string())
}

/// base64 in, files out, through the same `tar` the seeder uses at the other
/// end.
fn unpack(into: &Path, tar_b64: &str) -> Result<(), String> {
    use std::io::Write as _;
    use std::process::Stdio;

    let bytes = crate::files::decode_base64(tar_b64.trim())
        .ok_or_else(|| "the upload was not valid base64".to_string())?;

    let mut child = Command::new("tar")
        .arg("-xzf")
        .arg("-")
        .arg("-C")
        .arg(into)
        // `..` is refused rather than skipped, and an absolute member cannot
        // become one: this is a tar from another machine.
        .arg("--no-same-owner")
        .arg("--no-same-permissions")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run tar: {e}"))?;
    child
        .stdin
        .as_mut()
        .ok_or("tar took no input")?
        .write_all(&bytes)
        .map_err(|e| format!("could not write to tar: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("tar did not finish: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "the upload could not be unpacked: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
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

    /// The round trip a client makes: pack from `~/.claude/skills` here, install
    /// into the server's library there, and hand the next session the result.
    #[test]
    fn a_skill_travels_from_a_client_into_the_library() {
        let (root, dir) = a_skill(
            "ship-pr",
            &[("SKILL.md", "# ship\n"), ("scripts/go.sh", "echo hi\n")],
        );
        // What the client sees when it looks at its own skills directory.
        let mine = local_in(&root);
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].name, "ship-pr");

        let library = root.join("library");
        fs::create_dir_all(&library).unwrap();
        let upload = Upload {
            name: mine[0].name.clone(),
            origin: dir.display().to_string(),
            tar: payload(&dir).unwrap(),
        };
        let stored = install(&library, &upload).expect("installed");
        assert_eq!(stored.name, "ship-pr");
        assert_eq!(stored.origin, dir.display().to_string());
        assert!(stored.uploaded_at > 0);

        // The whole directory, not just the manifest: a skill is its scripts too.
        assert_eq!(
            fs::read_to_string(library.join("ship-pr/scripts/go.sh")).unwrap(),
            "echo hi\n"
        );
        // And the record of where it came from, which a session's own record
        // would otherwise have to call "the library".
        let listed = library_at(&library);
        assert_eq!(listed, vec![stored.clone()]);
        // Beside the skills, never inside one: everything inside a skill
        // directory is copied into the sandbox.
        assert!(library.join(ORIGINS).is_file());
        assert!(!library.join("ship-pr").join(ORIGINS).exists());

        // A second upload replaces it, and a file deleted in between goes.
        fs::remove_file(dir.join("scripts/go.sh")).unwrap();
        install(&library, &upload_of(&dir)).unwrap();
        assert!(
            !library.join("ship-pr/scripts/go.sh").exists(),
            "unpacking over the old copy would keep a deleted file for ever"
        );

        forget(&library, "ship-pr").unwrap();
        assert!(library_at(&library).is_empty());
        assert!(!library.join("ship-pr").exists());
        // Twice is fine.
        forget(&library, "ship-pr").unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    fn upload_of(dir: &Path) -> Upload {
        Upload {
            name: dir.file_name().unwrap().to_string_lossy().into_owned(),
            origin: dir.display().to_string(),
            tar: payload(dir).unwrap(),
        }
    }

    /// **A tar arrives from another machine.** Each of these is checked in a
    /// staging directory and refused, rather than landing in the directory
    /// every future session copies from.
    #[test]
    fn an_upload_that_is_not_one_skill_is_refused() {
        let (root, dir) = a_skill("good", &[("SKILL.md", "# good\n")]);
        let library = root.join("library");
        fs::create_dir_all(&library).unwrap();

        // A name that is a path, which is the whole class worth refusing by
        // shape: it decides a directory here and inside every sandbox.
        for bad in ["../escape", "a/b", "/abs", "..", ".hidden", ""] {
            let err = install(
                &library,
                &Upload {
                    name: bad.to_string(),
                    origin: "x".into(),
                    tar: payload(&dir).unwrap(),
                },
            )
            .expect_err(bad);
            assert!(
                err.contains("not a skill name") || err.contains("needs a name"),
                "{bad}: {err}"
            );
        }

        // A payload whose contents are not what it says they are.
        let err = install(
            &library,
            &Upload {
                name: "renamed".into(),
                origin: "x".into(),
                tar: payload(&dir).unwrap(),
            },
        )
        .expect_err("a mismatched name");
        assert!(err.contains("not what it says it is"), "{err}");

        // A directory with no manifest is not a skill the agent would load.
        let (other_root, other) = a_skill("empty", &[("notes.txt", "hi")]);
        let err = install(&library, &upload_of(&other)).expect_err("no manifest");
        assert!(err.contains("SKILL.md"), "{err}");

        // Nothing was left behind by any of it.
        assert!(library_at(&library).is_empty());
        assert!(
            !library.join("escape").exists() && !root.join("escape").exists(),
            "a staged upload must not survive being refused"
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&other_root);
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
