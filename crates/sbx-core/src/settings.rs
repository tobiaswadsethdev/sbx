//! The config file's editable defaults, written back from a client.
//!
//! [`crate::config`] reads the file; this changes it. The split is not
//! bookkeeping. Reading is `deny_unknown_fields` against a struct shaped like
//! the file, and writing is a *text* edit that has to come back out of that
//! same parser identical in every respect except the keys asked for -- two
//! different problems, and only one of them is allowed to lose a comment.
//!
//! **Nothing here regenerates the file.** Serialising a parsed `Raw` back over
//! it would be four lines and would throw away every comment in it, and the
//! comments are most of what `sbxd config --init` writes: the file's job is to
//! document the defaults as much as to change them, and a settings screen that
//! silently deleted the documentation would make the next person's `sbxd
//! config --init` a destructive command. So each managed key is found and
//! replaced or removed where it stands, and every other byte is passed
//! through.
//!
//! **The result is parsed before it is written.** The edit below is a
//! line-oriented one, which is what preserving the file costs; the guarantee
//! that it cannot leave an unloadable config behind comes from running the new
//! text through [`Config::parse`] and refusing to write it if it does not
//! survive. That is also where the *meaning* is checked -- a policy that is
//! not a template, a `refresh` outside its bounds -- so this file does not
//! have a second opinion about any of it.

use std::fs;
use std::io;
use std::ops::Range;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{self, Config, Error};
use crate::session;

/// The defaults a client may change.
///
/// A subset of the file, and the subset is "what a new session starts with"
/// plus the one switch about the server itself. `repo_roots`, `worktree_root`,
/// `skills` and `[[mcp]]` are deliberately not here: each is a decision about
/// what an agent of yours can reach or where its files land, and a text field
/// in a window is the wrong shape for any of it.
///
/// `[[tracker]]` used to be on that list and is not any more. The argument was
/// that a list of tables is the wrong shape for a settings screen, which is
/// true and is why it is not one: [`add_tracker`] and [`forget_tracker`] add
/// and remove whole tables from the integrations screen, beside the secret each
/// one needs. Without them the inbox is a pane that can only ever be empty for
/// anyone who does not edit the server's config file by hand -- which, from a
/// desktop on another machine, is nobody.
///
/// Every field is an `Option`, and `None` means **the key is not in the file**
/// rather than "empty". That difference is the whole reason for the type: an
/// absent key gets the built-in default, so taking `policy` out is not the
/// same as writing down whichever template happens to be the default today.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// What a work branch is named under: `<prefix>/<name>`.
    pub branch_prefix: Option<String>,
    /// The branch a new session clones from. `None` means the remote's own
    /// default, which is almost always right.
    pub base: Option<String>,
    /// Policy template name, or a path to a YAML file.
    pub policy: Option<String>,
    /// Credential providers attached to a new session.
    pub providers: Option<Vec<String>>,
    /// Whether `sbxd serve` may fetch a newer release in the background.
    pub auto_update: Option<bool>,
}

/// What a settings screen is drawn from.
///
/// The file's answers, where the file is, and the built-in default behind the
/// one field whose absence a person needs to see spelled out -- a blank branch
/// prefix field has to say `sbx` somewhere or it reads as "no prefix".
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    /// The file, whether or not it exists. A screen that writes a file has to
    /// be able to say which one.
    pub path: String,
    /// Whether there is one yet. The difference between "these are the
    /// defaults" and "this is what you asked for", which is the same
    /// distinction [`Config::present`] exists for.
    pub present: bool,
    pub settings: Settings,
    /// [`session::DEFAULT_BRANCH_PREFIX`], for the placeholder.
    pub default_branch_prefix: String,
}

impl From<&Config> for Settings {
    fn from(cfg: &Config) -> Self {
        Settings {
            branch_prefix: cfg.branch_prefix.clone(),
            base: cfg.base.clone(),
            policy: cfg.policy.clone(),
            providers: cfg.providers.clone(),
            auto_update: cfg.auto_update,
        }
    }
}

impl Settings {
    /// Blanks and empty lists become "not in the file".
    ///
    /// A cleared text field and an absent key are the same intent -- go back to
    /// the default -- and `policy = ""` would be a line the parser turns
    /// straight back into `None` anyway, so writing one would leave the file
    /// saying something it does not mean.
    ///
    /// `providers` is the one the parser actively *refuses*: `providers = []`
    /// is rejected because in a hand-written file it reads as a mistake. But
    /// unticking every box is not a mistake, and the two are the same thing
    /// where the value is used -- [`Config::providers`] answers an empty slice
    /// for an absent key too -- so an empty list removes the key.
    pub fn normalized(&self) -> Settings {
        let text = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Settings {
            branch_prefix: text(&self.branch_prefix),
            base: text(&self.base),
            policy: text(&self.policy),
            providers: self
                .providers
                .as_ref()
                .map(|list| {
                    list.iter()
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect::<Vec<_>>()
                })
                .filter(|list| !list.is_empty()),
            auto_update: self.auto_update,
        }
    }
}

/// The file as a settings screen should draw it.
pub fn view(cfg: &Config) -> SettingsView {
    SettingsView {
        path: cfg.path.display().to_string(),
        present: cfg.present,
        settings: Settings::from(cfg),
        default_branch_prefix: session::DEFAULT_BRANCH_PREFIX.to_string(),
    }
}

/// Put these settings in the file, and answer with the config as it now reads.
///
/// The config comes back rather than an acknowledgement for the reason the
/// integrations screen gives about its own view: what the file *now says* is
/// not what was sent -- a blank came out as an absent key, and an absent key
/// reads as the built-in default -- and a client adjusting the copy it already
/// had would be inventing the answer.
///
/// A file that is not there yet is created from [`config::EXAMPLE`], the same
/// text `sbxd config --init` writes. Saving one field should not produce a
/// two-line config file that documents nothing, and the example's commented-out
/// keys are also what [`insertion_point`] aims at.
pub fn save(path: &Path, settings: &Settings) -> Result<Config, Error> {
    let current = read_or_example(path)?;

    let next = edit(&current, &settings.normalized());
    // Before the write, not after: see the module note. A file that does not
    // parse is one every command except `sbx doctor` refuses to run against,
    // so writing one from a settings screen would break the server from
    // inside its own UI.
    let cfg = Config::parse(path, &next)?;
    write_atomically(path, &next)?;
    Ok(cfg)
}

/// Add a `[[tracker]]` table, and answer with the config as it now reads.
///
/// Appended rather than merged into whatever is already there: a table is the
/// unit here, the file is read in order, and appending is the one edit that
/// cannot disturb a line somebody else wrote.
///
/// Everything about whether the tracker makes sense -- a Jira entry with no
/// site, a name two trackers share, a kind nobody has heard of -- is
/// [`Config::parse`]'s answer rather than a second opinion here, which is the
/// same division [`save`] works on. So an entry that would break the file is
/// refused in the parser's own words and nothing is written.
pub fn add_tracker(path: &Path, source: &crate::tracker::Source) -> Result<Config, Error> {
    let current = read_or_example(path)?;
    let newline = if current.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push_str(newline);
    }
    next.push_str(&table(&source.normalized(), newline));

    let cfg = Config::parse(path, &next)?;
    write_atomically(path, &next)?;
    Ok(cfg)
}

/// Take one out by name, and answer with the config as it now reads.
///
/// The name is resolved against the *parsed* config and the table is then
/// removed by position, rather than by looking for a `name = ` line: a tracker
/// with no `name` key is named after its kind by the parser, and a textual
/// search would not find the entry it is being asked to remove.
pub fn forget_tracker(path: &Path, name: &str) -> Result<Config, Error> {
    let current = read_or_example(path)?;
    let cfg = Config::parse(path, &current)?;
    let missing = |message: String| Error::Invalid {
        path: path.to_path_buf(),
        key: "tracker",
        message,
    };
    let Some(index) = cfg.trackers().iter().position(|t| t.name == name) else {
        return Err(missing(format!("no tracker called `{name}`")));
    };

    let newline = if current.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut lines: Vec<String> = current
        .split('\n')
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let Some(extent) = nth_table(&lines, "[[tracker]]", index) else {
        return Err(missing(format!(
            "`{name}` is in the config as it parsed but not in its text"
        )));
    };
    lines.splice(extent, None);
    let mut next = lines.join(newline);
    next.push_str(newline);

    let cfg = Config::parse(path, &next)?;
    write_atomically(path, &next)?;
    Ok(cfg)
}

/// The file, or the documented example when there is not one yet.
fn read_or_example(path: &Path) -> Result<String, Error> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(config::EXAMPLE.to_string()),
        Err(source) => Err(Error::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// One tracker as the text of a table, with the blank line that separates it
/// from whatever is above.
fn table(source: &crate::tracker::Source, newline: &str) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push_str(newline);
    };
    line(String::new());
    line("[[tracker]]".to_string());
    line(format!("kind = {}", quote(source.kind.label())));
    line(format!("name = {}", quote(&source.name)));
    line(format!("secret = {}", quote(&source.secret)));
    for (key, value) in [
        ("repo", &source.repo),
        ("org", &source.org),
        ("project", &source.project),
        ("site", &source.site),
        ("email", &source.email),
        ("query", &source.query),
        ("on_publish", &source.on_publish),
    ] {
        if let Some(value) = value {
            line(format!("{key} = {}", quote(value)));
        }
    }
    out
}

/// The lines the `n`th table with this header occupies, the blank lines above
/// it included.
///
/// It ends where the next table begins, which is what a table *is* in TOML. The
/// blank lines above come with it so that adding and removing a tracker leaves
/// the file as it was rather than a growing gap where one used to be.
fn nth_table(lines: &[String], header: &str, n: usize) -> Option<Range<usize>> {
    let mut seen = 0;
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if depth == 0 && line.trim_start().starts_with('[') {
            if let Some(start) = start {
                return Some(blank_before(lines, start)..i);
            }
            if line.trim() == header {
                if seen == n {
                    start = Some(i);
                } else {
                    seen += 1;
                }
            }
        }
        depth += brackets(line);
    }
    start.map(|start| blank_before(lines, start)..lines.len())
}

/// How far back the blank lines immediately above a line run.
fn blank_before(lines: &[String], start: usize) -> usize {
    let mut first = start;
    while first > 0 && lines[first - 1].trim().is_empty() {
        first -= 1;
    }
    first
}

/// Write and then rename, so a crash or a full disk leaves the old file rather
/// than half of a new one. The config is read by every command; a truncated
/// one is a server that will not start.
fn write_atomically(path: &Path, text: &str) -> Result<(), Error> {
    let failed = |source: io::Error| Error::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(failed)?;
    }
    // Beside the file, because a rename across filesystems is not atomic and
    // /tmp is often another one.
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".new");
    let tmp = std::path::PathBuf::from(tmp);
    fs::write(&tmp, text).map_err(failed)?;
    fs::rename(&tmp, path).map_err(|source| {
        // The temporary is this function's litter and nobody else's; leaving
        // it would make the next save look like it had a stale file to read.
        let _ = fs::remove_file(&tmp);
        failed(source)
    })
}

/// The file with the five managed keys set to what `settings` says, and
/// nothing else touched.
fn edit(text: &str, settings: &Settings) -> String {
    // Line endings are preserved as a whole rather than per line: a file is
    // one or the other, and a save that left LF among CRLF would show up as a
    // diff on every line in whatever editor sees it next.
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect();
    // Splitting a text that ends in a newline leaves an empty last element.
    // Dropped here and added back by the join, so a file does not grow a blank
    // line every time it is saved.
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }

    set(
        &mut lines,
        "branch_prefix",
        settings.branch_prefix.as_deref().map(quote),
    );
    set(&mut lines, "base", settings.base.as_deref().map(quote));
    set(&mut lines, "policy", settings.policy.as_deref().map(quote));
    set(
        &mut lines,
        "providers",
        settings.providers.as_deref().map(array),
    );
    set(
        &mut lines,
        "auto_update",
        settings.auto_update.map(|on| on.to_string()),
    );

    let mut out = lines.join(newline);
    out.push_str(newline);
    out
}

/// Set one key, or take it out when there is no value.
fn set(lines: &mut Vec<String>, key: &str, value: Option<String>) {
    // Recomputed per key rather than once, because an insertion moves the
    // region's end.
    let region = top_level(lines);
    let line = value.map(|v| format!("{key} = {v}"));
    match assignment(lines, key, region.clone()) {
        Some(extent) => {
            lines.splice(extent, line).for_each(drop);
        }
        None => {
            if let Some(line) = line {
                lines.insert(insertion_point(lines, key, region), line);
            }
        }
    }
}

/// The lines before the first table header, which is the only place a
/// top-level key can live: everything after `[[mcp]]` belongs to `[[mcp]]`.
///
/// Bracket depth is tracked so that `repo_roots = [` continued over three
/// lines is not mistaken for a table header on whichever of them starts with a
/// bracket. Multi-line strings are not tracked and do not need to be: every
/// key the file accepts up here is a scalar or an array of them, so a `"""`
/// block in this region is a file the parser rejects before this runs.
fn top_level(lines: &[String]) -> Range<usize> {
    let mut depth = 0i32;
    for (i, line) in lines.iter().enumerate() {
        if depth == 0 && line.trim_start().starts_with('[') {
            return 0..i;
        }
        depth += brackets(line);
    }
    0..lines.len()
}

/// The lines an active `key = ...` occupies, if the region has one.
fn assignment(lines: &[String], key: &str, region: Range<usize>) -> Option<Range<usize>> {
    let start = region.clone().find(|&i| assigns(&lines[i], key))?;
    // An array may run over several lines, and replacing only the first of
    // them would leave `"~/dev",` behind as a line of its own.
    let mut depth = brackets(&lines[start]);
    let mut end = start + 1;
    while depth > 0 && end < region.end {
        depth += brackets(&lines[end]);
        end += 1;
    }
    Some(start..end)
}

/// Where a key that is not in the file yet should go.
///
/// Under its own documentation, when the file has some. `sbxd config --init`
/// writes every key commented out with a paragraph above it saying what it
/// does, and a value that lands directly beneath that paragraph is one the
/// next reader can make sense of -- which is most of what keeps this file worth
/// having after a settings screen has written to it.
///
/// Failing that, after the last answer already in the file; failing that, at
/// the end of the top-level region. Never after a table header, where the key
/// would silently stop being a top-level key at all.
fn insertion_point(lines: &[String], key: &str, region: Range<usize>) -> usize {
    let mut documented = None;
    let mut after_last = None;

    let mut i = region.start;
    while i < region.end {
        if documented.is_none() && documents(&lines[i], key) {
            documented = Some(i);
        }
        if assigns_anything(&lines[i]) {
            let mut depth = brackets(&lines[i]);
            let mut end = i + 1;
            while depth > 0 && end < region.end {
                depth += brackets(&lines[end]);
                end += 1;
            }
            after_last = Some(end);
            i = end;
            continue;
        }
        i += 1;
    }

    if let Some(i) = documented {
        return i + 1;
    }
    if let Some(i) = after_last {
        return i;
    }
    // Nothing follows the region, so the end of the file is simply the end of
    // the file.
    if region.end == lines.len() {
        return region.end;
    }
    // Something does. The run of comments and blank lines above a table header
    // introduces that table, and a key dropped between the two would read as
    // though it belonged to it.
    let mut at = region.end;
    while at > region.start {
        let line = lines[at - 1].trim();
        if line.is_empty() || line.starts_with('#') {
            at -= 1;
        } else {
            break;
        }
    }
    at
}

/// `key = ...`, uncommented.
///
/// The prefix has to be followed by the `=` and nothing else, or `base` would
/// match `base_branch` -- and it has to be uncommented, because the example
/// file is a whole page of `# base = "develop"` and treating one of those as
/// an answer is how a settings screen reports a default the file never set.
fn assigns(line: &str, key: &str) -> bool {
    line.trim_start()
        .strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// `# key = ...`: the commented-out example, which is where a new value goes.
fn documents(line: &str, key: &str) -> bool {
    line.trim_start()
        .strip_prefix('#')
        .is_some_and(|rest| assigns(rest, key))
}

/// Any active assignment, whichever key it is for.
fn assigns_anything(line: &str) -> bool {
    let line = line.trim_start();
    !line.is_empty() && !line.starts_with('#') && line.contains('=')
}

/// How far this line opens or closes an array, ignoring brackets inside a
/// string or a trailing comment.
fn brackets(line: &str) -> i32 {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in line.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match (quote, c) {
            // Only a basic string has escapes; a literal one is literal.
            (Some('"'), '\\') => escaped = true,
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') => break,
            (None, '[') => depth += 1,
            (None, ']') => depth -= 1,
            (None, _) => {}
        }
    }
    depth
}

/// A TOML basic string.
///
/// Written here rather than taken from the `toml` crate because that
/// dependency is built with `parse` and without `display`: this file is read
/// with a parser and, outside these five keys, never written at all. Adding a
/// serialiser to the build to quote five strings would be the larger change.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // The rest of the control range has no shorthand and may not go in
            // raw. Nothing a person types into these fields reaches here; a
            // paste out of a terminal can.
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// An array of them, on one line. The file's own examples are written that way
/// and two providers do not need three lines.
fn array(values: &[String]) -> String {
    let items: Vec<String> = values.iter().map(|v| quote(v)).collect();
    format!("[{}]", items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix(p: &str) -> Settings {
        Settings {
            branch_prefix: Some(p.to_string()),
            ..Settings::default()
        }
    }

    #[test]
    fn replaces_an_answer_the_file_already_has() {
        let before = "# what a branch is called\nbranch_prefix = \"sbx\"\nrepo = \"x\"\n";
        assert_eq!(
            edit(before, &prefix("tobias")),
            "# what a branch is called\nbranch_prefix = \"tobias\"\nrepo = \"x\"\n"
        );
    }

    #[test]
    fn removes_a_key_rather_than_blanking_it() {
        // `policy = ""` parses back to `None`, so writing one would leave the
        // file saying something it does not mean.
        let before = "policy = \"feature-work\"\nbase = \"main\"\n";
        // `base` is carried through, because a `None` field means "take the
        // key out" and this test is about `policy`.
        let cleared = Settings {
            base: Some("main".into()),
            policy: Some("   ".into()),
            ..Settings::default()
        };
        assert_eq!(edit(before, &cleared.normalized()), "base = \"main\"\n");
    }

    #[test]
    fn lands_under_its_own_documentation() {
        let before = "# gateway = \"default\"\n\n# the prefix\n# branch_prefix = \"tobias\"\n";
        assert_eq!(
            edit(before, &prefix("tobias")),
            "# gateway = \"default\"\n\n# the prefix\n# branch_prefix = \"tobias\"\nbranch_prefix = \"tobias\"\n"
        );
    }

    #[test]
    fn a_commented_key_is_not_an_answer() {
        // The example file is a page of these. Read as answers, a settings
        // screen would report defaults the file never set -- and this edit
        // would replace the documentation instead of adding a line.
        assert!(!assigns("# base = \"develop\"", "base"));
        assert!(documents("# base = \"develop\"", "base"));
        assert!(assigns("base = \"develop\"", "base"));
    }

    #[test]
    fn a_longer_key_is_not_this_one() {
        assert!(!assigns("base_branch = \"main\"", "base"));
        assert!(!assigns("branch_prefix = \"x\"", "branch"));
    }

    #[test]
    fn never_writes_a_top_level_key_into_a_table() {
        let table = "[[mcp]]\nname = \"jira\"\nurl = \"http://mcp-atlassian:9000/mcp\"\n";
        let before = format!("repo = \"https://example.com/x\"\n\n# the servers\n{table}");
        let after = edit(&before, &prefix("tobias"));
        assert_eq!(
            after,
            format!(
                "repo = \"https://example.com/x\"\nbranch_prefix = \"tobias\"\n\n# the servers\n{table}"
            )
        );
        // The point of the test is the *parse*: the key has to still be a
        // top-level one afterwards.
        let cfg = Config::parse(Path::new("x.toml"), &after).unwrap();
        assert_eq!(cfg.branch_prefix.as_deref(), Some("tobias"));
        assert_eq!(cfg.mcp.len(), 1);
    }

    #[test]
    fn inserts_above_the_paragraph_that_introduces_a_table() {
        // With no answer and no documentation to aim at, the fallback still
        // may not split a comment from the header it belongs to.
        let before = "# the servers\n# and how they work\n[[mcp]]\nname = \"jira\"\nurl = \"http://x:1/mcp\"\n";
        assert_eq!(
            edit(before, &prefix("tobias")),
            "branch_prefix = \"tobias\"\n# the servers\n# and how they work\n[[mcp]]\nname = \"jira\"\nurl = \"http://x:1/mcp\"\n"
        );
    }

    #[test]
    fn replaces_an_array_that_runs_over_several_lines() {
        let before = "providers = [\n  \"claude-oauth\",\n  \"azure-pat\",\n]\nrepo = \"x\"\n";
        let one = Settings {
            providers: Some(vec!["claude-oauth".into()]),
            ..Settings::default()
        };
        assert_eq!(
            edit(before, &one),
            "providers = [\"claude-oauth\"]\nrepo = \"x\"\n"
        );
    }

    #[test]
    fn an_empty_provider_list_removes_the_key() {
        // `providers = []` is the one value `Config::parse` refuses outright.
        let before = "providers = [\"claude-oauth\"]\n";
        let none = Settings {
            providers: Some(Vec::new()),
            ..Settings::default()
        };
        let after = edit(before, &none.normalized());
        assert_eq!(after, "\n");
        Config::parse(Path::new("x.toml"), &after).unwrap();
    }

    #[test]
    fn keeps_the_files_line_endings() {
        let before = "gateway = \"default\"\r\nrepo = \"x\"\r\n";
        assert_eq!(
            edit(before, &prefix("tobias")),
            "gateway = \"default\"\r\nrepo = \"x\"\r\nbranch_prefix = \"tobias\"\r\n"
        );
    }

    #[test]
    fn saving_twice_changes_nothing_the_second_time() {
        let once = edit(config::EXAMPLE, &prefix("tobias"));
        let twice = edit(&once, &prefix("tobias"));
        assert_eq!(once, twice);
    }

    #[test]
    fn every_managed_key_survives_a_round_trip_through_the_example() {
        let settings = Settings {
            branch_prefix: Some("tobias".into()),
            base: Some("develop".into()),
            policy: Some("feature-work".into()),
            providers: Some(vec!["claude-oauth".into(), "azure-pat".into()]),
            auto_update: Some(false),
        };
        let text = edit(config::EXAMPLE, &settings);
        let cfg = Config::parse(Path::new("x.toml"), &text).unwrap();
        assert_eq!(Settings::from(&cfg), settings);
        // And the documentation is still there, which is the whole reason this
        // is a text edit and not a re-serialisation.
        assert!(text.contains("# sbx defaults."));
        assert!(text.contains("# repo_roots = [\"~/dev\", \"~/work\"]"));
    }

    #[test]
    fn the_example_keeps_every_comment_when_nothing_is_set() {
        // A save with everything cleared is a real case -- somebody turning the
        // screen back to the defaults -- and it must not empty the file.
        let text = edit(config::EXAMPLE, &Settings::default());
        assert_eq!(text, config::EXAMPLE);
    }

    #[test]
    fn a_hostile_value_comes_back_out_as_itself() {
        // Not a branch name any validator would accept; the point is that the
        // parser reads back exactly what went in rather than a broken file.
        let settings = Settings {
            policy: Some("/etc/\"quoted\"\\path.yaml".into()),
            ..Settings::default()
        };
        let text = edit("", &settings);
        let cfg = Config::parse(Path::new("x.toml"), &text).unwrap();
        assert_eq!(cfg.policy.as_deref(), Some("/etc/\"quoted\"\\path.yaml"));
    }

    #[test]
    fn brackets_ignores_a_comment_and_a_string() {
        assert_eq!(brackets("providers = ["), 1);
        assert_eq!(brackets("providers = []"), 0);
        assert_eq!(brackets("repo = \"http://x/[a]\""), 0);
        assert_eq!(brackets("base = \"main\" # [[mcp]]"), 0);
        assert_eq!(brackets("policy = \"a\\\"[\""), 0);
    }

    #[test]
    fn writes_and_reloads() {
        let dir = std::env::temp_dir().join(format!("sbx-settings-{}", std::process::id()));
        let path = dir.join("config.toml");
        let _ = fs::remove_dir_all(&dir);

        // No file yet: the first save creates the documented one.
        let cfg = save(&path, &prefix("tobias")).unwrap();
        assert_eq!(cfg.branch_prefix(), "tobias");
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("# sbx defaults.")
        );

        // And the second one edits it in place.
        let cfg = save(&path, &prefix("someone-else")).unwrap();
        assert_eq!(cfg.branch_prefix(), "someone-else");
        assert_eq!(
            Config::load_from(&path).unwrap().branch_prefix(),
            "someone-else"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    fn tracker_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sbx-tracker-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn jira(name: &str) -> crate::tracker::Source {
        crate::tracker::Source {
            kind: crate::tracker::Kind::Jira,
            name: name.into(),
            secret: "JIRA_TOKEN".into(),
            site: Some("https://you.atlassian.net".into()),
            email: Some("you@example.invalid".into()),
            ..Default::default()
        }
    }

    /// The whole point of the pair: a tracker can be added and taken away
    /// again, and the inbox has something to read in between.
    #[test]
    fn a_tracker_is_added_and_removed_leaving_the_file_as_it_was() {
        let dir = tracker_dir("round-trip");
        let path = dir.join("config.toml");
        let before = concat!(
            "# my server\n",
            "branch_prefix = \"tobias\"\n",
            "\n",
            "[[mcp]]\n",
            "name = \"jira\"\n",
            "url = \"http://mcp-jira:9000/mcp\"\n",
        );
        fs::write(&path, before).unwrap();

        let cfg = add_tracker(&path, &jira("work")).unwrap();
        assert_eq!(cfg.trackers().len(), 1);
        assert_eq!(cfg.trackers()[0].name, "work");
        assert_eq!(
            cfg.trackers()[0].site.as_deref(),
            Some("https://you.atlassian.net")
        );
        // Appended, so the `[[mcp]]` table above is untouched and the comment
        // at the top is still the first line.
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(before), "{after}");
        assert!(after.contains("[[tracker]]"), "{after}");

        let cfg = forget_tracker(&path, "work").unwrap();
        assert!(cfg.trackers().is_empty());
        // Byte for byte: the blank line that came with the table goes with it,
        // or a tracker added and removed a few times leaves a growing gap.
        assert_eq!(fs::read_to_string(&path).unwrap(), before);

        let _ = fs::remove_dir_all(&dir);
    }

    /// Two of a kind, removed by name. A tracker with no `name` key is named
    /// after its kind by the parser, so the one to remove is found by asking
    /// the parsed config which it is and cutting that table.
    #[test]
    fn the_right_one_of_two_is_removed() {
        let dir = tracker_dir("two");
        let path = dir.join("config.toml");
        fs::write(&path, "").unwrap();

        add_tracker(&path, &jira("mine")).unwrap();
        add_tracker(&path, &jira("theirs")).unwrap();
        let cfg = forget_tracker(&path, "mine").unwrap();

        let left: Vec<&str> = cfg.trackers().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(left, ["theirs"]);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("theirs"), "{text}");
        assert!(!text.contains("mine"), "{text}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// The parser is the only opinion about whether an entry can work, and it
    /// runs before the write: a Jira tracker with no site is refused and the
    /// file is left as the file it was.
    #[test]
    fn a_tracker_that_could_not_work_is_refused_and_nothing_is_written() {
        let dir = tracker_dir("refused");
        let path = dir.join("config.toml");
        fs::write(&path, "base = \"main\"\n").unwrap();

        let mut bad = jira("work");
        bad.site = None;
        assert!(add_tracker(&path, &bad).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "base = \"main\"\n");

        // And a name two trackers share, which is the other way an inbox ends
        // up writing a comment back to the wrong tracker.
        add_tracker(&path, &jira("work")).unwrap();
        assert!(
            add_tracker(&path, &jira("work")).is_err(),
            "a second `work` must be refused"
        );
        assert_eq!(
            Config::load_from(&path).unwrap().trackers().len(),
            1,
            "and not written"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// An unnamed tracker is named after its kind, and the fields belonging to
    /// another kind are not written at all -- a GitHub entry carrying a `site`
    /// key is one the parser refuses.
    #[test]
    fn a_table_carries_only_what_its_kind_uses() {
        let source = crate::tracker::Source {
            kind: crate::tracker::Kind::GitHub,
            name: "  ".into(),
            secret: " GITHUB_TOKEN ".into(),
            repo: Some("octocat/Hello-World".into()),
            ..Default::default()
        };
        let text = table(&source.normalized(), "\n");
        assert!(text.contains("kind = \"github\""), "{text}");
        assert!(text.contains("name = \"github\""), "{text}");
        assert!(text.contains("secret = \"GITHUB_TOKEN\""), "{text}");
        assert!(text.contains("repo = \"octocat/Hello-World\""), "{text}");
        assert!(!text.contains("site"), "{text}");
        assert!(!text.contains("query"), "{text}");
    }

    #[test]
    fn a_rejected_value_leaves_the_file_alone() {
        let dir = std::env::temp_dir().join(format!("sbx-settings-bad-{}", std::process::id()));
        let path = dir.join("config.toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "base = \"main\"\n").unwrap();

        let bad = Settings {
            policy: Some("not-a-template".into()),
            ..Settings::default()
        };
        assert!(save(&path, &bad).is_err());
        // The check happens before the write, so the old file is not half a
        // new one -- it is the old file.
        assert_eq!(fs::read_to_string(&path).unwrap(), "base = \"main\"\n");

        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod real_file {
    //! A round trip over whatever config file this machine actually has.
    //!
    //! Ignored, because the suite is hermetic and a developer's home directory
    //! is not a fixture. Run by hand -- `cargo test -p sbx-core real_file --
    //! --ignored --nocapture` -- when changing the editor above, because the
    //! example file is the case that was designed for and a file somebody has
    //! actually edited is the case that has to work.
    use super::*;

    #[test]
    #[ignore = "reads the developer's own config file"]
    fn the_local_config_survives_a_save() {
        let path = Config::default_path();
        let Ok(text) = fs::read_to_string(&path) else {
            eprintln!("no config file at {}; nothing to check", path.display());
            return;
        };
        let before = Config::parse(&path, &text).expect("the local config parses");
        let settings = Settings::from(&before);

        // Saving what is already there must be a no-op, byte for byte.
        let same = edit(&text, &settings.normalized());
        assert_eq!(same, text, "saving an unchanged config rewrote it");

        // And a real change must leave everything else alone.
        let changed = Settings {
            branch_prefix: Some("round-trip".into()),
            ..settings.clone()
        };
        let after = edit(&text, &changed.normalized());
        let reparsed = Config::parse(&path, &after).expect("the edited config parses");
        assert_eq!(reparsed.branch_prefix.as_deref(), Some("round-trip"));
        assert_eq!(reparsed.mcp, before.mcp);
        assert_eq!(reparsed.trackers, before.trackers);
        assert_eq!(reparsed.skills, before.skills);
        assert_eq!(reparsed.repo_roots, before.repo_roots);
        eprintln!(
            "ok: {} lines in, {} out",
            text.lines().count(),
            after.lines().count()
        );
    }

    /// The same question for the table edits, on a copy: adding a tracker to a
    /// file somebody has actually written, and taking it out again, has to give
    /// that file back unchanged.
    #[test]
    #[ignore = "reads the developer's own config file"]
    fn a_tracker_round_trips_through_the_local_config() {
        let Ok(text) = fs::read_to_string(Config::default_path()) else {
            eprintln!("no config file; nothing to check");
            return;
        };
        // A copy, because these two write: the point is the round trip, not the
        // developer's file.
        let dir = std::env::temp_dir().join(format!("sbx-real-tracker-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, &text).unwrap();

        let source = crate::tracker::Source {
            kind: crate::tracker::Kind::GitHub,
            name: "round-trip".into(),
            secret: "GITHUB_TOKEN".into(),
            ..Default::default()
        };
        let with = add_tracker(&path, &source).expect("the local config takes a tracker");
        assert!(with.trackers().iter().any(|t| t.name == "round-trip"));
        forget_tracker(&path, "round-trip").expect("and gives it back");
        assert_eq!(fs::read_to_string(&path).unwrap(), text);

        let _ = fs::remove_dir_all(&dir);
        eprintln!("ok: a tracker came and went without touching the rest");
    }
}
