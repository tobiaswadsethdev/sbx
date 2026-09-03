//! Projects: a repository someone has decided to work on.
//!
//! A worktree is created *inside* a project rather than from a repository
//! picked afresh each time, which is the difference between a list of sessions
//! and a workspace. Picking the repository was the first question of every
//! create; making it a standing answer means the question asked when starting
//! work is the one that varies -- what this worktree is for.
//!
//! **A project is a decision, not a discovery.** `repos::discover_in` finds
//! every checkout on the machine, which is tens of them; a project is the
//! handful someone has said they are working on. That is why this is stored
//! rather than derived from the sessions that exist: a project with no
//! worktrees in it yet is the normal state of one you have just made, and
//! grouping sessions by their clone URL could never represent it.
//!
//! What it holds is a clone URL and the checkout it was named from. The URL is
//! what a sandbox clones; the path is what `Inspect` reads to say which branch
//! is checked out and what has drifted. Two projects may share a URL -- two
//! checkouts of the same repository is a normal thing to have -- which is why a
//! worktree records the project it belongs to rather than being matched back to
//! one by URL.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A repository someone is working on.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Unique, and what a worktree records to say where it belongs. Derived
    /// from the folder unless one was given.
    pub name: String,
    /// Clone URL. Every worktree in the project starts from this.
    pub repo: String,
    /// The checkout on the server this was named from, as a path.
    pub path: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_at: u64,
}

/// A project a client is asking for.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewProject {
    pub path: String,
    /// Clone URL, which the picker already knows: it is `origin` of the
    /// checkout. Sent rather than re-read so a project cannot end up pointing
    /// at a different remote from the one that was on screen.
    pub repo: String,
    /// `None` to name it after the folder.
    pub name: Option<String>,
}

fn default_path() -> PathBuf {
    crate::store::Store::default_path().with_file_name("projects.json")
}

/// Every project, oldest first.
pub fn list() -> Vec<Project> {
    list_at(&default_path())
}

/// As with [`crate::comments`], each of these has a twin taking the file: the
/// store lives in the config directory, and a test that had to move
/// `XDG_CONFIG_HOME` to reach it would race every other test doing the same.
pub fn list_at(path: &Path) -> Vec<Project> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write(path: &Path, projects: &[Project]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(projects).map_err(|e| e.to_string())?;
    fs::write(path, text).map_err(|e| format!("could not write {}: {e}", path.display()))
}

pub fn add(new: NewProject) -> Result<Vec<Project>, String> {
    add_at(&default_path(), new)
}

pub fn add_at(path: &Path, new: NewProject) -> Result<Vec<Project>, String> {
    // A checkout with no origin has nothing for a sandbox to clone. Refused
    // here rather than at the first worktree, which is a long way from the
    // decision that caused it.
    if new.repo.trim().is_empty() {
        return Err("that checkout has no origin, so nothing could be cloned from it".into());
    }
    let mut all = list_at(path);
    if let Some(existing) = all.iter().find(|p| p.path == new.path) {
        return Err(format!(
            "that folder is already the project `{}`",
            existing.name
        ));
    }

    let wanted = new
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| folder_name(&new.path));
    let name = unique(&wanted, &all);

    all.push(Project {
        name,
        repo: new.repo,
        path: new.path,
        created_at: crate::session::now_epoch(),
    });
    write(path, &all)?;
    Ok(all)
}

/// Forget a project. The worktrees in it are left alone: a sandbox is a real
/// thing with an agent in it, and removing one is `sbx rm`'s job, said out loud.
pub fn remove(name: &str) -> Result<Vec<Project>, String> {
    remove_at(&default_path(), name)
}

pub fn remove_at(path: &Path, name: &str) -> Result<Vec<Project>, String> {
    let mut all = list_at(path);
    all.retain(|p| p.name != name);
    write(path, &all)?;
    Ok(all)
}

/// The last segment of a path, as a person would say it.
fn folder_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "project".to_string())
}

/// `name`, `name-2`, `name-3` ... Two checkouts of one repository are a normal
/// thing to have, and they cannot both be called `sbx`.
fn unique(wanted: &str, existing: &[Project]) -> String {
    if !existing.iter().any(|p| p.name == wanted) {
        return wanted.to_string();
    }
    (2..)
        .map(|n| format!("{wanted}-{n}"))
        .find(|candidate| !existing.iter().any(|p| &p.name == candidate))
        .unwrap_or_else(|| wanted.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sbx-projects-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("projects.json")
    }

    fn draft(path: &str, repo: &str) -> NewProject {
        NewProject {
            path: path.into(),
            repo: repo.into(),
            name: None,
        }
    }

    #[test]
    fn a_project_is_named_after_its_folder_and_round_trips() {
        let store = scratch("roundtrip");
        let all = add_at(&store, draft("/home/x/dev/sbx", "https://h/o/sbx.git")).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "sbx");
        assert_eq!(list_at(&store)[0].repo, "https://h/o/sbx.git");
    }

    /// Two checkouts of one repository is a normal thing to have, and they
    /// cannot both be called `sbx`.
    #[test]
    fn a_second_checkout_of_the_same_repository_gets_its_own_name() {
        let store = scratch("dupe");
        add_at(&store, draft("/a/sbx", "https://h/o/sbx.git")).unwrap();
        let all = add_at(&store, draft("/b/sbx", "https://h/o/sbx.git")).unwrap();
        assert_eq!(all[1].name, "sbx-2");
    }

    /// The same folder twice is a mistake, not a second project, and saying so
    /// beats two entries that behave identically.
    #[test]
    fn the_same_folder_twice_is_refused_by_name() {
        let store = scratch("same");
        add_at(&store, draft("/a/sbx", "https://h/o/sbx.git")).unwrap();
        let err = add_at(&store, draft("/a/sbx", "https://h/o/sbx.git")).unwrap_err();
        assert!(err.contains("already the project `sbx`"), "{err}");
    }

    /// A checkout with no origin cannot be cloned from, so it is refused at the
    /// decision rather than at the first worktree.
    #[test]
    fn a_checkout_with_no_origin_is_refused() {
        let store = scratch("noorigin");
        assert!(add_at(&store, draft("/a/thing", "")).is_err());
        assert!(list_at(&store).is_empty());
    }

    #[test]
    fn forgetting_a_project_leaves_the_others() {
        let store = scratch("forget");
        add_at(&store, draft("/a/one", "https://h/o/one.git")).unwrap();
        add_at(&store, draft("/b/two", "https://h/o/two.git")).unwrap();
        let all = remove_at(&store, "one").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "two");
    }
}
