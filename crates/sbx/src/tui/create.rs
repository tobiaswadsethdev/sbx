//! Starting a session from the TUI: pick a repository, then fill in the rest.
//!
//! Two stages rather than one screen, because they answer different questions.
//! The picker answers *which repository*, which is a search; the form answers
//! *what kind of session*, which is a handful of fields with defaults good
//! enough to press enter on. Both are pure state machines driven by key events
//! and hold no I/O: the scan, the git inspection, the provider list and the
//! create itself all happen on the worker, so a slow disk or an unreachable
//! gateway can never block a keystroke.
//!
//! The repository is a way of naming a *remote*. The sandbox clones `origin`
//! over the gateway exactly as `sbx new --repo <url>` does, so a checkout with
//! no origin cannot start a session, and local edits and unpushed commits stay
//! on the host -- which is why the form shows how many of each there are.

use openshell_client::Provider;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use sbx_core::ops;
use sbx_core::policy;
use sbx_core::repos::{Facts, LocalRepo};
use sbx_core::session;
use sbx_core::toolchain;

/// A single-line text field with a cursor.
///
/// The cursor is a *character* index, not a byte offset: a task description is
/// free text and may hold anything, and indexing a String by bytes is a panic
/// waiting for the first non-ASCII character someone pastes in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    text: String,
    cursor: usize,
}

impl Input {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Input { text, cursor }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Cursor position in characters from the start, for the renderer.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Replace the contents, cursor to the end. Used for fields the form keeps
    /// deriving until they are edited by hand.
    pub fn set(&mut self, text: impl Into<String>) {
        *self = Input::new(text);
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte offset of a character index, for splicing.
    fn byte_of(&self, index: usize) -> usize {
        self.text
            .char_indices()
            .nth(index)
            .map_or(self.text.len(), |(b, _)| b)
    }

    /// Handle a key, reporting whether it belonged to the field.
    ///
    /// Returning `false` is what lets the form bind Tab, Enter and Escape:
    /// anything the field does not claim falls through to the form.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Readline's kill-line and kill-word, because this is a terminal
            // and muscle memory for them is universal.
            KeyCode::Char('u') if ctrl => {
                let at = self.byte_of(self.cursor);
                self.text.replace_range(..at, "");
                self.cursor = 0;
                true
            }
            KeyCode::Char('w') if ctrl => {
                let start = self.word_start();
                let (from, to) = (self.byte_of(start), self.byte_of(self.cursor));
                self.text.replace_range(from..to, "");
                self.cursor = start;
                true
            }
            KeyCode::Char('a') if ctrl => {
                self.cursor = 0;
                true
            }
            KeyCode::Char('e') if ctrl => {
                self.cursor = self.len();
                true
            }
            // Alt-modified characters are left alone: they are not text, and
            // claiming them would swallow bindings added later.
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                let at = self.byte_of(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
                true
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let from = self.byte_of(self.cursor - 1);
                    let to = self.byte_of(self.cursor);
                    self.text.replace_range(from..to, "");
                    self.cursor -= 1;
                }
                true
            }
            KeyCode::Delete => {
                if self.cursor < self.len() {
                    let from = self.byte_of(self.cursor);
                    let to = self.byte_of(self.cursor + 1);
                    self.text.replace_range(from..to, "");
                }
                true
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.len());
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                true
            }
            KeyCode::End => {
                self.cursor = self.len();
                true
            }
            _ => false,
        }
    }

    /// Start of the word before the cursor, for Ctrl-W.
    fn word_start(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut at = self.cursor;
        while at > 0 && chars[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !chars[at - 1].is_whitespace() {
            at -= 1;
        }
        at
    }
}

/// The create flow, as far as it has got.
pub enum Create {
    Pick(Picker),
    Fill(Box<Form>),
}

/// What a key press did, for the event loop to act on.
pub enum Action {
    /// Handled; nothing else to do.
    None,
    /// Close the flow.
    Cancel,
    /// A repository was picked: inspect it and move to the form.
    Picked(Box<LocalRepo>),
    /// Back to the picker.
    Back,
    /// Create this session.
    Submit(Box<ops::Draft>),
}

/// Choosing a repository from the ones found on disk.
pub struct Picker {
    /// `None` until the scan comes back, which is what the pane shows as
    /// "scanning" rather than as "no repositories".
    repos: Option<Vec<LocalRepo>>,
    query: Input,
    /// Indices into `repos`, best match first.
    matches: Vec<usize>,
    cursor: usize,
    /// `repo` from the config file: the row to open on, if the scan finds it.
    /// Dropped as soon as anything is typed, since a filter is a better
    /// statement of intent than a file is.
    prefer: Option<String>,
    error: Option<String>,
}

impl Picker {
    pub fn new() -> Self {
        Picker::preferring(None)
    }

    /// A picker that opens on `prefer` -- a clone URL or a repository name --
    /// when the scan turns it up.
    ///
    /// The cursor rather than the filter: prefilling the query would hide every
    /// other repository behind a backspace, and the point of the picker is that
    /// the other ones are one keystroke away.
    pub fn preferring(prefer: Option<String>) -> Self {
        Picker {
            repos: None,
            query: Input::default(),
            matches: Vec::new(),
            cursor: 0,
            prefer,
            error: None,
        }
    }

    /// Take the scan's result.
    pub fn scanned(&mut self, repos: Vec<LocalRepo>) {
        self.repos = Some(repos);
        self.refilter();
        self.prefer();
    }

    /// Put the cursor on the preferred repository, once, if it is there.
    ///
    /// By origin URL first, since that is what the config file naturally holds
    /// and what identifies a repository unambiguously; by name second, so
    /// `repo = "sbx"` works too.
    fn prefer(&mut self) {
        let (Some(wanted), Some(repos)) = (self.prefer.as_deref(), self.repos.as_ref()) else {
            return;
        };
        let at = |f: &dyn Fn(&LocalRepo) -> bool| {
            self.matches
                .iter()
                .position(|i| repos.get(*i).is_some_and(f))
        };
        if let Some(row) = at(&|r: &LocalRepo| r.origin.as_deref() == Some(wanted))
            .or_else(|| at(&|r: &LocalRepo| r.name == wanted))
        {
            self.cursor = row;
        }
    }

    pub fn scanning(&self) -> bool {
        self.repos.is_none()
    }

    pub fn query(&self) -> &Input {
        &self.query
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// How many repositories were found, for the pane title.
    pub fn total(&self) -> usize {
        self.repos.as_ref().map_or(0, |r| r.len())
    }

    /// The rows to show, in match order.
    pub fn rows(&self) -> Vec<&LocalRepo> {
        let Some(repos) = &self.repos else {
            return Vec::new();
        };
        self.matches.iter().filter_map(|i| repos.get(*i)).collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selected(&self) -> Option<&LocalRepo> {
        let repos = self.repos.as_ref()?;
        repos.get(*self.matches.get(self.cursor)?)
    }

    fn refilter(&mut self) {
        let Some(repos) = &self.repos else {
            return;
        };
        self.matches = sbx_core::repos::filter(repos, self.query.text());
        // Back to the best match: keeping the old index as the query changes
        // would leave the cursor on whatever happened to land at that row.
        self.cursor = 0;
    }

    fn move_by(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = self.matches.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    /// A page for the picker, which the renderer measures; a fixed step is
    /// enough here since the list is short and always fully drawn.
    const PAGE: isize = 10;

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => return Action::Cancel,
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::PageUp => self.move_by(-Self::PAGE),
            KeyCode::PageDown => self.move_by(Self::PAGE),
            // Ctrl-N/Ctrl-P, so the list can be walked without leaving the
            // filter -- the arrows are a reach mid-word.
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => self.move_by(1),
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => self.move_by(-1),
            KeyCode::Enter => {
                let Some(repo) = self.selected().cloned() else {
                    self.error = Some(if self.scanning() {
                        "still scanning".to_string()
                    } else {
                        "no repository matches".to_string()
                    });
                    return Action::None;
                };
                // Refused here rather than at create time: without an origin
                // there is nothing for the sandbox to clone, and the form would
                // have nothing to show but the same complaint.
                if repo.origin.is_none() {
                    self.error = Some(format!(
                        "{} has no origin remote; the sandbox clones from one",
                        repo.name
                    ));
                    return Action::None;
                }
                return Action::Picked(Box::new(repo));
            }
            _ => {
                if self.query.on_key(key) {
                    self.error = None;
                    self.prefer = None;
                    self.refilter();
                }
            }
        }
        Action::None
    }
}

/// Which field the form's keys act on. The order is the Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Task,
    Name,
    Base,
    Policy,
    Toolchains,
    Providers,
}

impl Field {
    const ORDER: [Field; 6] = [
        Field::Task,
        Field::Name,
        Field::Base,
        Field::Policy,
        Field::Toolchains,
        Field::Providers,
    ];

    fn step(self, delta: isize) -> Field {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0) as isize;
        let len = Self::ORDER.len() as isize;
        Self::ORDER[((i + delta).rem_euclid(len)) as usize]
    }

    pub fn label(self) -> &'static str {
        match self {
            Field::Task => "task",
            Field::Name => "name",
            Field::Base => "base",
            Field::Policy => "policy",
            Field::Toolchains => "tools",
            Field::Providers => "providers",
        }
    }
}

/// One thing a session can be given, and whether it is.
///
/// Shared by the two multi-select fields, because they are the same widget over
/// different lists: `kind` is a provider's type in one and a toolchain's summary
/// in the other, and both are the dimmed text beside the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub name: String,
    pub kind: String,
    pub selected: bool,
}

/// What the config file has to say about a new session.
///
/// Passed in rather than read here, like everything else in this module: a form
/// is built on a keystroke, and reading a file on the render thread is the same
/// mistake as running a subprocess on it. `None` on a field means the file is
/// silent and the form's own answer stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Defaults {
    /// Only used when the checkout is on a detached HEAD. The branch a
    /// repository is actually sitting on is evidence about *that* repository;
    /// a config entry is a guess about all of them, and the evidence wins.
    pub base: Option<String>,
    /// A template name or a path to a YAML file.
    pub policy: Option<String>,
    /// Ticked instead of the guesswork in [`preselect`], not as well as it: an
    /// explicit list is a better answer than any heuristic, and merging the two
    /// would attach credentials nobody asked for.
    pub providers: Option<Vec<String>>,
    /// MCP servers every session gets. Carried through the form without being
    /// shown as a field, because there is nothing to choose: it is one decision
    /// about what your agents can reach, made in the config file. The form's job
    /// here is only to put it on the draft, so the TUI and `sbx new` cannot
    /// create sessions with different tools.
    pub mcp: Vec<sbx_core::mcp::Server>,
    /// Skills copied into the sandbox. Carried, not chosen, like `mcp`.
    pub skills: Vec<sbx_core::skills::Skill>,
}

/// One entry of the form's policy chooser.
///
/// A [`policy::Template`] for all but one case: a config file may name a YAML
/// file instead, and that has to be offered too or the TUI would quietly create
/// sessions under a different policy from `sbx new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyOption {
    /// What goes into the draft: a template name, or a path.
    pub spec: String,
    pub summary: String,
}

/// Every toolchain, with the ones the checkout points at already ticked.
///
/// All of them are listed rather than only the detected ones: the list is three
/// lines long, and a form that hid `dotnet` because the repository has no
/// `.csproj` yet would be a form you cannot use to start writing one.
fn toolchain_choices(detected: &[String]) -> Vec<Choice> {
    toolchain::TOOLCHAINS
        .iter()
        .map(|t| Choice {
            name: t.name.to_string(),
            kind: t.summary.to_string(),
            selected: detected.iter().any(|d| d == t.name),
        })
        .collect()
}

/// The templates, with a configured path in front of them when there is one.
fn policy_options(configured: Option<&str>) -> Vec<PolicyOption> {
    let mut out: Vec<PolicyOption> = Vec::new();
    if let Some(spec) = configured.filter(|s| policy::find(s).is_none()) {
        out.push(PolicyOption {
            spec: spec.to_string(),
            summary: "from your config file".to_string(),
        });
    }
    out.extend(policy::TEMPLATES.iter().map(|t| PolicyOption {
        spec: t.name.to_string(),
        summary: t.summary.to_string(),
    }));
    out
}

/// The fields of a session about to be created.
pub struct Form {
    pub repo: LocalRepo,
    /// `None` until git has been asked; the pane says so rather than claiming a
    /// clean tree.
    facts: Option<Facts>,
    task: Input,
    name: Input,
    /// What already exists, for keeping derived names clear of it.
    history: History,
    /// Whether the name has been typed into. Until it has, it follows the task,
    /// which is what makes the common case a single field to fill in.
    name_edited: bool,
    base: Input,
    policies: Vec<PolicyOption>,
    policy: usize,
    providers: Vec<Choice>,
    provider_cursor: usize,
    toolchains: Vec<Choice>,
    toolchain_cursor: usize,
    /// Whether the toolchain list has been touched. Until it has, it follows
    /// what the checkout turns out to contain -- git's answer arrives after the
    /// form is already open, and a tick appearing under the cursor a moment
    /// after it was cleared by hand would be the tool arguing.
    toolchains_edited: bool,
    /// The config file's provider list, kept because the gateway's own list may
    /// arrive after the form is open and has to be ticked the same way then.
    configured_providers: Option<Vec<String>>,
    mcp: Vec<sbx_core::mcp::Server>,
    skills: Vec<sbx_core::skills::Skill>,
    /// Why the provider list is empty, when the gateway could not be asked.
    providers_error: Option<String>,
    field: Field,
    error: Option<String>,
}

impl Form {
    /// Build a form for a picked repository.
    ///
    /// `providers` is what the gateway reported, or the reason it could not be
    /// asked. Passed in rather than fetched, because a form is created on a
    /// keystroke and this module does no I/O.
    ///
    /// `history` is what the sessions that already exist have to say: the names
    /// that are taken, and the credentials the last session for this repository
    /// was given. Both are things the form cannot work out for itself and both
    /// are the difference between a form that has to be corrected and one that
    /// is already right.
    pub fn new(
        repo: LocalRepo,
        providers: Option<&Result<Vec<Provider>, String>>,
        history: History,
        defaults: &Defaults,
    ) -> Self {
        let base = repo
            .branch
            .clone()
            .or_else(|| defaults.base.clone())
            .unwrap_or_default();
        let (choices, providers_error) = match providers {
            Some(Ok(list)) => (
                match &defaults.providers {
                    Some(named) => chosen(list, named),
                    None => preselect(list, &repo, &history.providers),
                },
                None,
            ),
            Some(Err(e)) => (Vec::new(), Some(e.clone())),
            None => (Vec::new(), Some("still reading the provider list".into())),
        };
        let policies = policy_options(defaults.policy.as_deref());

        let derived = session::derive_name("", &repo.name).unwrap_or_default();

        Form {
            facts: None,
            // Derived from the repository until there is a task to derive from,
            // and made unique so a second session in the same repository does not
            // have to be renamed by hand.
            name: Input::new(session::unique_name(&derived, &history.taken)),
            task: Input::default(),
            name_edited: false,
            base: Input::new(base),
            policy: default_policy_index(&policies, defaults.policy.as_deref()),
            policies,
            providers: choices,
            provider_cursor: 0,
            // Empty until `inspected` brings the checkout's answer: the form is
            // built on a keystroke and this module does no I/O, which is the same
            // reason the branch facts arrive late.
            toolchains: toolchain_choices(&[]),
            toolchain_cursor: 0,
            toolchains_edited: false,
            configured_providers: defaults.providers.clone(),
            mcp: defaults.mcp.clone(),
            skills: defaults.skills.clone(),
            providers_error,
            history,
            field: Field::Task,
            error: None,
            repo,
        }
    }

    /// Take git's answer about the picked repository.
    ///
    /// A base branch that does not exist on the remote is cleared rather than
    /// kept: `git clone --branch` would fail on it, and the remote's default
    /// branch is both a valid answer and almost certainly the intended one.
    pub fn inspected(&mut self, facts: Facts) {
        if !facts.base_on_remote && !self.base.is_empty() {
            self.base.set("");
        }
        // Only while the list is untouched, so an answer arriving late cannot
        // undo a choice made in the meantime -- the same rule the provider list
        // follows in `providers_arrived`.
        if !self.toolchains_edited {
            self.toolchains = toolchain_choices(&facts.toolchains);
        }
        self.facts = Some(facts);
    }

    /// Take a provider list that arrived after the form was opened.
    pub fn providers_arrived(&mut self, providers: &Result<Vec<Provider>, String>) {
        // Only if nothing has been chosen yet, so a list landing late cannot
        // undo a selection made in the meantime.
        if !self.providers.is_empty() {
            return;
        }
        match providers {
            Ok(list) => {
                self.providers = match &self.configured_providers {
                    Some(named) => chosen(list, named),
                    None => preselect(list, &self.repo, &self.history.providers),
                };
                self.providers_error = None;
            }
            Err(e) => self.providers_error = Some(e.clone()),
        }
    }

    pub fn facts(&self) -> Option<&Facts> {
        self.facts.as_ref()
    }

    pub fn field(&self) -> Field {
        self.field
    }

    pub fn input(&self, field: Field) -> Option<&Input> {
        match field {
            Field::Task => Some(&self.task),
            Field::Name => Some(&self.name),
            Field::Base => Some(&self.base),
            Field::Policy | Field::Toolchains | Field::Providers => None,
        }
    }

    pub fn policy(&self) -> &PolicyOption {
        &self.policies[self.policy.min(self.policies.len() - 1)]
    }

    pub fn providers(&self) -> &[Choice] {
        &self.providers
    }

    pub fn provider_cursor(&self) -> usize {
        self.provider_cursor
    }

    pub fn providers_error(&self) -> Option<&str> {
        self.providers_error.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Report a problem the form could not know about itself, e.g. a name the
    /// session list already holds.
    pub fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
    }

    fn cycle_policy(&mut self, delta: isize) {
        let len = self.policies.len() as isize;
        self.policy = ((self.policy as isize + delta).rem_euclid(len)) as usize;
    }

    pub fn toolchains(&self) -> &[Choice] {
        &self.toolchains
    }

    pub fn toolchain_cursor(&self) -> usize {
        self.toolchain_cursor
    }

    fn move_toolchain(&mut self, delta: isize) {
        if self.toolchains.is_empty() {
            return;
        }
        let last = self.toolchains.len() as isize - 1;
        self.toolchain_cursor = (self.toolchain_cursor as isize + delta).clamp(0, last) as usize;
    }

    fn toggle_toolchain(&mut self) {
        if let Some(c) = self.toolchains.get_mut(self.toolchain_cursor) {
            c.selected = !c.selected;
            self.toolchains_edited = true;
        }
    }

    fn move_provider(&mut self, delta: isize) {
        if self.providers.is_empty() {
            return;
        }
        let last = self.providers.len() as isize - 1;
        self.provider_cursor = (self.provider_cursor as isize + delta).clamp(0, last) as usize;
    }

    fn toggle_provider(&mut self) {
        if let Some(c) = self.providers.get_mut(self.provider_cursor) {
            c.selected = !c.selected;
        }
    }

    /// The draft this form describes, or why it is not ready.
    pub fn draft(&self) -> Result<ops::Draft, String> {
        let name = self.name.text().trim().to_string();
        session::validate_name(&name).map_err(|e| e.to_string())?;
        let base = self.base.text().trim();
        Ok(ops::Draft {
            name,
            // Checked by the picker, so an empty string here is unreachable;
            // it would fail the create rather than doing something surprising.
            repo: self.repo.origin.clone().unwrap_or_default(),
            task: self.task.text().trim().to_string(),
            base: (!base.is_empty()).then(|| base.to_string()),
            policy: self.policy().spec.clone(),
            providers: self
                .providers
                .iter()
                .filter(|c| c.selected)
                .map(|c| c.name.clone())
                .collect(),
            mcp: self.mcp.clone(),
            skills: self.skills.clone(),
            // Resolved here rather than carried as names, so the draft the TUI
            // hands `ops::create` is the same shape `sbx new` hands it. A name
            // that does not resolve is unreachable -- the list is built from
            // `toolchain::TOOLCHAINS` -- and is dropped rather than failing a
            // create over a list this module wrote itself.
            toolchains: self
                .toolchains
                .iter()
                .filter(|c| c.selected)
                .filter_map(|c| toolchain::find(&c.name))
                .collect(),
            start: true,
        })
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        // Field-local keys first, so a text field keeps its characters and the
        // two chooser fields get the keys a text field would otherwise eat.
        match self.field {
            Field::Task | Field::Name | Field::Base => {
                let mut input = std::mem::take(match self.field {
                    Field::Task => &mut self.task,
                    Field::Name => &mut self.name,
                    _ => &mut self.base,
                });
                let claimed = input.on_key(key);
                match self.field {
                    Field::Task => self.task = input,
                    Field::Name => self.name = input,
                    _ => self.base = input,
                }
                if claimed {
                    self.error = None;
                    match self.field {
                        // Typing in the name pins it; typing in the task
                        // re-derives it until then.
                        Field::Name => self.name_edited = true,
                        Field::Task if !self.name_edited => {
                            let derived = session::derive_name(self.task.text(), &self.repo.name)
                                .unwrap_or_default();
                            self.name
                                .set(session::unique_name(&derived, &self.history.taken));
                        }
                        _ => {}
                    }
                    return Action::None;
                }
            }
            Field::Policy => match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    self.cycle_policy(-1);
                    return Action::None;
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                    self.cycle_policy(1);
                    return Action::None;
                }
                _ => {}
            },
            Field::Toolchains => match key.code {
                KeyCode::Char(' ') => {
                    self.toggle_toolchain();
                    return Action::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_toolchain(-1);
                    return Action::None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_toolchain(1);
                    return Action::None;
                }
                _ => {}
            },
            Field::Providers => match key.code {
                KeyCode::Char(' ') => {
                    self.toggle_provider();
                    return Action::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_provider(-1);
                    return Action::None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_provider(1);
                    return Action::None;
                }
                _ => {}
            },
        }

        match key.code {
            // Back to the picker rather than out of the flow: the most likely
            // reason to escape a form is having picked the wrong repository.
            // Escaping the picker then closes everything, so two presses always
            // leave.
            KeyCode::Esc => Action::Back,
            KeyCode::Tab | KeyCode::Down => {
                self.field = self.field.step(1);
                Action::None
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.field = self.field.step(-1);
                Action::None
            }
            KeyCode::Enter => match self.draft() {
                Ok(draft) => Action::Submit(Box::new(draft)),
                Err(e) => {
                    self.error = Some(e);
                    // Put the cursor where the problem is, so the complaint is
                    // actionable without hunting for the field.
                    self.field = Field::Name;
                    Action::None
                }
            },
            _ => Action::None,
        }
    }
}

/// Which providers a new session should start with checked.
///
/// Two, at most: the one carrying the agent's credential, and the one carrying
/// a credential for the repository's host. Both only when the *type* identifies
/// exactly one provider -- with two Azure PATs there is no way to know which
/// organisation is meant, and guessing would attach a credential that cannot
/// authenticate and produce a failure three steps later.
/// What the sessions that already exist tell the form.
#[derive(Debug, Clone, Default)]
pub struct History {
    /// Session names in use, so a derived one can avoid them.
    pub taken: Vec<String>,
    /// Provider names the most recent session for this repository's host was
    /// given. A record of what worked, not a guess.
    pub providers: Vec<String>,
}

/// Which credentials to tick, as [`Choice`]es for the chooser.
///
/// The rule itself is [`ops::preselect_providers`], in the core, because the
/// window's create form needs the same answer and a session started from either
/// has to arrive with the same credentials. This is only the shape the chooser
/// wants it in.
fn preselect(providers: &[Provider], repo: &LocalRepo, used_before: &[String]) -> Vec<Choice> {
    let choices: Vec<ops::ProviderChoice> = providers
        .iter()
        .map(|p| ops::ProviderChoice {
            name: p.name.clone(),
            kind: p.kind.clone(),
        })
        .collect();
    let ticked = ops::preselect_providers(&choices, repo.origin.as_deref(), used_before);
    providers
        .iter()
        .map(|p| Choice {
            selected: ticked.contains(&p.name),
            name: p.name.clone(),
            kind: p.kind.clone(),
        })
        .collect()
}

/// Which entry the chooser opens on: the configured one, else the default
/// template.
fn default_policy_index(options: &[PolicyOption], configured: Option<&str>) -> usize {
    let wanted = configured.unwrap_or(policy::DEFAULT_TEMPLATE);
    options
        .iter()
        .position(|o| o.spec == wanted)
        .unwrap_or_default()
}

/// Tick exactly the providers the config file names, and nothing else.
///
/// A name that no longer exists at the gateway simply does not appear, which is
/// the only thing that can be done from here -- `sbx doctor` is where a stale
/// name gets said out loud, because it is the command that can ask.
fn chosen(providers: &[Provider], named: &[String]) -> Vec<Choice> {
    providers
        .iter()
        .map(|p| Choice {
            selected: named.iter().any(|n| n == &p.name),
            name: p.name.clone(),
            kind: p.kind.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(input: &mut Input, text: &str) {
        for c in text.chars() {
            assert!(input.on_key(key(KeyCode::Char(c))), "must claim {c:?}");
        }
    }

    fn repo(name: &str, origin: Option<&str>, branch: Option<&str>) -> LocalRepo {
        LocalRepo {
            path: format!("/home/u/dev/{name}").into(),
            display: format!("~/dev/{name}"),
            name: name.to_string(),
            origin: origin.map(String::from),
            branch: branch.map(String::from),
        }
    }

    fn provider(name: &str, kind: &str) -> Provider {
        Provider {
            name: name.to_string(),
            kind: kind.to_string(),
            credential_keys: Vec::new(),
        }
    }

    #[test]
    fn input_edits_at_the_cursor() {
        let mut i = Input::default();
        typed(&mut i, "hello");
        assert_eq!(i.text(), "hello");
        i.on_key(key(KeyCode::Left));
        i.on_key(key(KeyCode::Left));
        typed(&mut i, "X");
        assert_eq!(i.text(), "helXlo");
        i.on_key(key(KeyCode::Backspace));
        assert_eq!(i.text(), "hello");
        i.on_key(key(KeyCode::Home));
        i.on_key(key(KeyCode::Delete));
        assert_eq!(i.text(), "ello");
        i.on_key(key(KeyCode::End));
        typed(&mut i, "!");
        assert_eq!(i.text(), "ello!");
    }

    /// Byte indexing would panic here, and a task description is exactly the
    /// field someone pastes an em dash or an accent into.
    #[test]
    fn input_handles_multibyte_text() {
        let mut i = Input::new("naïve — ok");
        i.on_key(key(KeyCode::Backspace));
        assert_eq!(i.text(), "naïve — o");
        i.on_key(key(KeyCode::Home));
        i.on_key(key(KeyCode::Right));
        i.on_key(key(KeyCode::Right));
        i.on_key(key(KeyCode::Delete));
        assert_eq!(i.text(), "nave — o", "the ï is one character, not two");
    }

    #[test]
    fn input_supports_readline_kills() {
        let mut i = Input::new("fix the readme typo");
        assert!(i.on_key(ctrl('w')));
        assert_eq!(i.text(), "fix the readme ");
        assert!(i.on_key(ctrl('u')));
        assert_eq!(i.text(), "");

        let mut i = Input::new("abc");
        i.on_key(ctrl('a'));
        assert_eq!(i.cursor(), 0);
        i.on_key(ctrl('e'));
        assert_eq!(i.cursor(), 3);
    }

    #[test]
    fn input_leaves_unclaimed_keys_alone() {
        let mut i = Input::new("x");
        for code in [
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Up,
            KeyCode::Down,
        ] {
            assert!(!i.on_key(key(code)), "{code:?} belongs to the form");
        }
        assert_eq!(i.text(), "x");
    }

    #[test]
    fn picker_filters_as_the_query_is_typed() {
        let mut p = Picker::new();
        assert!(p.scanning(), "no rows until the scan lands");
        p.scanned(vec![
            repo("api", Some("u"), Some("main")),
            repo("web", Some("u"), Some("main")),
        ]);
        assert_eq!(p.rows().len(), 2);

        p.on_key(key(KeyCode::Char('w')));
        assert_eq!(p.rows().len(), 1);
        assert_eq!(p.selected().map(|r| r.name.as_str()), Some("web"));
    }

    #[test]
    fn picker_cursor_clamps_and_resets_on_a_new_query() {
        let mut p = Picker::new();
        p.scanned(vec![
            repo("aa", Some("u"), Some("main")),
            repo("ab", Some("u"), Some("main")),
        ]);
        p.on_key(key(KeyCode::Down));
        p.on_key(key(KeyCode::Down));
        assert_eq!(p.cursor(), 1, "must not run past the end");
        p.on_key(key(KeyCode::Char('a')));
        assert_eq!(p.cursor(), 0, "a new query means a new best match");
        p.on_key(key(KeyCode::Up));
        assert_eq!(p.cursor(), 0, "must not run past the top");
    }

    /// The config file's `repo` moves the cursor, not the filter: prefilling
    /// the query would hide every other repository behind a backspace.
    #[test]
    fn picker_opens_on_the_configured_repository() {
        let mut p = Picker::preferring(Some("https://github.com/o/web".into()));
        p.scanned(vec![
            repo("api", Some("https://github.com/o/api"), Some("main")),
            repo("web", Some("https://github.com/o/web"), Some("main")),
        ]);
        assert_eq!(p.selected().map(|r| r.name.as_str()), Some("web"));
        assert_eq!(p.rows().len(), 2, "the others are still one keystroke away");

        // By name too, since that is how anyone would write it by hand.
        let mut p = Picker::preferring(Some("web".into()));
        p.scanned(vec![
            repo("api", Some("https://github.com/o/api"), Some("main")),
            repo("web", Some("https://github.com/o/web"), Some("main")),
        ]);
        assert_eq!(p.selected().map(|r| r.name.as_str()), Some("web"));

        // A preference the scan did not turn up changes nothing.
        let mut p = Picker::preferring(Some("nowhere".into()));
        p.scanned(vec![repo("api", Some("u"), Some("main"))]);
        assert_eq!(p.selected().map(|r| r.name.as_str()), Some("api"));
    }

    /// The picker opens on the cached scan and a fresh one lands behind it,
    /// calling `scanned` again -- possibly after a query has been typed.
    /// Reapplying the preference then would pull the cursor off whatever was
    /// just filtered for, so typing drops it: a filter is a more specific
    /// statement of intent than a config file.
    #[test]
    fn a_rescan_reapplies_the_preference_until_something_is_typed() {
        let found = || {
            vec![
                repo("api", Some("https://github.com/o/api"), Some("main")),
                repo("web", Some("https://github.com/o/web"), Some("main")),
            ]
        };
        let mut p = Picker::preferring(Some("web".into()));
        p.scanned(found());

        // A rescan with nothing typed still lands on the configured repository.
        p.on_key(key(KeyCode::Up));
        p.scanned(found());
        assert_eq!(p.selected().map(|r| r.name.as_str()), Some("web"));

        // Once there is a query, the preference is gone for good -- including
        // for every later rescan, which is the case it exists to survive.
        p.on_key(key(KeyCode::Char('e')));
        assert!(p.prefer.is_none(), "typing is the more specific answer");
        p.scanned(found());
        assert_eq!(
            p.selected().map(|r| r.name.to_string()),
            p.rows().first().map(|r| r.name.to_string()),
            "the cursor belongs to the filter now"
        );
    }

    #[test]
    fn picker_refuses_a_repository_with_no_origin() {
        let mut p = Picker::new();
        p.scanned(vec![repo("solo", None, Some("main"))]);
        let action = p.on_key(key(KeyCode::Enter));
        assert!(matches!(action, Action::None));
        assert!(
            p.error().unwrap().contains("no origin"),
            "got {:?}",
            p.error()
        );

        // And typing again clears the complaint rather than leaving it up.
        p.on_key(key(KeyCode::Char('x')));
        assert!(p.error().is_none());
    }

    #[test]
    fn picker_enter_on_an_empty_scan_says_so() {
        let mut p = Picker::new();
        assert!(matches!(p.on_key(key(KeyCode::Enter)), Action::None));
        assert_eq!(p.error(), Some("still scanning"));
        p.scanned(vec![]);
        p.on_key(key(KeyCode::Enter));
        assert_eq!(p.error(), Some("no repository matches"));
    }

    #[test]
    fn picker_escape_closes_the_flow() {
        let mut p = Picker::new();
        assert!(matches!(p.on_key(key(KeyCode::Esc)), Action::Cancel));
    }

    #[test]
    fn picking_yields_the_repository() {
        let mut p = Picker::new();
        p.scanned(vec![repo(
            "api",
            Some("https://github.com/o/api.git"),
            None,
        )]);
        match p.on_key(key(KeyCode::Enter)) {
            Action::Picked(r) => assert_eq!(r.name, "api"),
            _ => panic!("expected a pick"),
        }
    }

    fn form() -> Form {
        let providers = Ok(vec![
            provider("claude-oauth", "claude-code-oauth"),
            provider("azure-pat", "azure-devops-pat"),
        ]);
        Form::new(
            repo("api", Some("https://github.com/o/api.git"), Some("main")),
            Some(&providers),
            History::default(),
            &Defaults::default(),
        )
    }

    #[test]
    fn the_name_follows_the_task_until_it_is_edited() {
        let mut f = form();
        // With no task yet, the repository's own name is the best guess.
        assert_eq!(f.input(Field::Name).unwrap().text(), "api");

        for c in "fix the readme".chars() {
            f.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(f.input(Field::Name).unwrap().text(), "fix-readme");

        // Editing the name pins it, and further task edits leave it alone.
        f.on_key(key(KeyCode::Tab));
        assert_eq!(f.field(), Field::Name);
        f.on_key(key(KeyCode::Char('2')));
        assert_eq!(f.input(Field::Name).unwrap().text(), "fix-readme2");
        f.on_key(key(KeyCode::BackTab));
        f.on_key(key(KeyCode::Char('!')));
        assert_eq!(
            f.input(Field::Name).unwrap().text(),
            "fix-readme2",
            "a hand-edited name must survive further typing in the task"
        );
    }

    #[test]
    fn tab_cycles_the_fields_both_ways() {
        let mut f = form();
        let order = [
            Field::Name,
            Field::Base,
            Field::Policy,
            Field::Toolchains,
            Field::Providers,
            Field::Task,
        ];
        for expected in order {
            f.on_key(key(KeyCode::Tab));
            assert_eq!(f.field(), expected);
        }
        f.on_key(key(KeyCode::BackTab));
        assert_eq!(f.field(), Field::Providers);
    }

    /// The whole point of detecting: a .NET repository comes up with dotnet
    /// already ticked, so the common case is a form to press Enter on.
    #[test]
    fn the_toolchains_arrive_ticked_from_the_checkout() {
        let mut f = form();
        // Nothing until git has answered, and every toolchain listed either way.
        assert_eq!(f.toolchains().len(), toolchain::TOOLCHAINS.len());
        assert!(f.toolchains().iter().all(|c| !c.selected));
        assert!(f.draft().unwrap().toolchains.is_empty());

        f.inspected(Facts {
            uncommitted: 0,
            unpushed: None,
            base_on_remote: true,
            toolchains: vec!["dotnet".into()],
        });
        let ticked: Vec<&str> = f
            .toolchains()
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(ticked, ["dotnet"]);
        assert_eq!(
            f.draft()
                .unwrap()
                .toolchains
                .iter()
                .map(|t| t.name)
                .collect::<Vec<_>>(),
            ["dotnet"]
        );
    }

    /// A guess is a guess: unticking has to hold, including against an answer
    /// from git that arrives afterwards.
    #[test]
    fn a_toolchain_chosen_by_hand_survives_a_late_answer() {
        let mut f = form();
        while f.field() != Field::Toolchains {
            f.on_key(key(KeyCode::Tab));
        }
        // The list is cursored and toggled like the provider list beside it.
        f.on_key(key(KeyCode::Char('j')));
        f.on_key(key(KeyCode::Char(' ')));
        assert_eq!(
            f.field(),
            Field::Toolchains,
            "space must not leave the field"
        );
        let picked: Vec<&str> = f
            .toolchains()
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(picked, ["node"], "the second row, cursored down to");

        f.inspected(Facts {
            uncommitted: 0,
            unpushed: None,
            base_on_remote: true,
            toolchains: vec!["rust".into()],
        });
        assert_eq!(
            f.draft()
                .unwrap()
                .toolchains
                .iter()
                .map(|t| t.name)
                .collect::<Vec<_>>(),
            ["node"],
            "a detection landing late must not undo a choice"
        );
    }

    #[test]
    fn the_policy_field_cycles_the_templates() {
        let mut f = form();
        assert_eq!(f.policy().spec, policy::DEFAULT_TEMPLATE);
        while f.field() != Field::Policy {
            f.on_key(key(KeyCode::Tab));
        }
        f.on_key(key(KeyCode::Right));
        assert_ne!(f.policy().spec, policy::DEFAULT_TEMPLATE);
        f.on_key(key(KeyCode::Left));
        assert_eq!(f.policy().spec, policy::DEFAULT_TEMPLATE);
        // And the arrows must not leave the field.
        assert_eq!(f.field(), Field::Policy);
    }

    /// The agent's credential and the repository host's, and nothing else: a
    /// session with no model credential comes up to a login prompt, and one
    /// with a stray credential attached has more reach than it needs.
    #[test]
    fn providers_are_preselected_by_type() {
        let f = form();
        let selected: Vec<&str> = f
            .providers()
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            selected,
            vec!["claude-oauth"],
            "github is the forge here, and no github provider exists"
        );
    }

    #[test]
    fn an_ambiguous_provider_type_is_left_unselected() {
        let providers = Ok(vec![
            provider("claude-oauth", "claude-code-oauth"),
            provider("azure-pat", "azure-devops-pat"),
            provider("azure-pat-personal", "azure-devops-pat"),
        ]);
        let azure = || {
            repo(
                "api",
                Some("https://dev.azure.com/org/proj/_git/api"),
                Some("main"),
            )
        };
        let ticked = |f: &Form| -> Vec<String> {
            f.providers()
                .iter()
                .filter(|c| c.selected)
                .map(|c| c.name.clone())
                .collect()
        };

        // Nothing to go on: the type alone cannot say which organisation was
        // meant, and the wrong PAT fails three steps later.
        let f = Form::new(
            azure(),
            Some(&providers),
            History::default(),
            &Defaults::default(),
        );
        assert_eq!(ticked(&f), vec!["claude-oauth".to_string()]);

        // With a session for this host already using one, that is the answer.
        let history = History {
            taken: vec!["api".into()],
            providers: vec!["azure-pat-personal".into(), "claude-oauth".into()],
        };
        let f = Form::new(azure(), Some(&providers), history, &Defaults::default());
        assert_eq!(
            ticked(&f),
            vec!["claude-oauth".to_string(), "azure-pat-personal".to_string()],
            "the credential the last session for this host was given"
        );
    }

    /// The config file is an explicit answer, so it replaces the guesswork
    /// entirely rather than adding to it. Merging the two would attach a
    /// credential nobody asked for, which is the failure `preselect` is careful
    /// to avoid.
    #[test]
    fn configured_providers_replace_the_guesswork() {
        let providers = Ok(vec![
            provider("claude-oauth", "claude-code-oauth"),
            provider("azure-pat", "azure-devops-pat"),
            provider("gh-token", "github-pat"),
        ]);
        let ticked = |f: &Form| -> Vec<String> {
            f.providers()
                .iter()
                .filter(|c| c.selected)
                .map(|c| c.name.clone())
                .collect()
        };
        let defaults = Defaults {
            providers: Some(vec!["gh-token".into(), "not-a-provider".into()]),
            ..Defaults::default()
        };

        // `claude-oauth` is the only provider of the agent's type and would be
        // ticked by `preselect`; the config says otherwise, so it is not.
        let f = Form::new(
            repo(
                "api",
                Some("https://dev.azure.com/org/proj/_git/api"),
                Some("main"),
            ),
            Some(&providers),
            History::default(),
            &defaults,
        );
        assert_eq!(ticked(&f), vec!["gh-token".to_string()]);

        // A name the gateway does not have simply is not offered. `sbx doctor`
        // is where that gets said out loud.
        assert!(f.providers().iter().all(|c| c.name != "not-a-provider"));
    }

    /// The gateway's provider list often lands after the form is already open,
    /// and it has to be ticked by the same rule as one that was there first.
    #[test]
    fn a_late_provider_list_still_honours_the_config() {
        let defaults = Defaults {
            providers: Some(vec!["gh-token".into()]),
            ..Defaults::default()
        };
        let mut f = Form::new(
            repo(
                "api",
                Some("https://dev.azure.com/org/proj/_git/api"),
                Some("main"),
            ),
            None,
            History::default(),
            &defaults,
        );
        assert!(f.providers().is_empty());
        f.providers_arrived(&Ok(vec![
            provider("claude-oauth", "claude-code-oauth"),
            provider("gh-token", "github-pat"),
        ]));
        let ticked: Vec<&str> = f
            .providers()
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(ticked, ["gh-token"]);
    }

    #[test]
    fn a_configured_policy_is_what_the_form_opens_on() {
        let defaults = Defaults {
            policy: Some("readonly-explore".into()),
            ..Defaults::default()
        };
        let f = Form::new(
            repo(
                "api",
                Some("https://dev.azure.com/org/proj/_git/api"),
                Some("main"),
            ),
            None,
            History::default(),
            &defaults,
        );
        assert_eq!(f.policy().spec, "readonly-explore");
    }

    /// A policy that is a *path* is not one of the templates, so the chooser has
    /// to carry it as an extra entry -- otherwise the TUI would quietly create
    /// sessions under a different policy from `sbx new`.
    #[test]
    fn a_configured_policy_path_is_offered_alongside_the_templates() {
        let defaults = Defaults {
            policy: Some("/etc/sbx/strict.yaml".into()),
            ..Defaults::default()
        };
        let mut f = Form::new(
            repo(
                "api",
                Some("https://dev.azure.com/org/proj/_git/api"),
                Some("main"),
            ),
            None,
            History::default(),
            &defaults,
        );
        assert_eq!(f.policy().spec, "/etc/sbx/strict.yaml");
        assert_eq!(f.draft().unwrap().policy, "/etc/sbx/strict.yaml");

        // And the templates are still reachable, in front of it and behind it.
        f.field = Field::Policy;
        f.on_key(key(KeyCode::Right));
        assert_eq!(f.policy().spec, policy::TEMPLATES[0].name);
        f.on_key(key(KeyCode::Left));
        assert_eq!(f.policy().spec, "/etc/sbx/strict.yaml");
        f.on_key(key(KeyCode::Left));
        assert_eq!(
            f.policy().spec,
            policy::TEMPLATES[policy::TEMPLATES.len() - 1].name,
            "the chooser wraps around the whole list, path included"
        );
    }

    /// The branch a checkout is sitting on is evidence about *that* repository;
    /// a config entry is a guess about all of them. The evidence wins, and the
    /// guess is only reached for when there is none -- a detached HEAD.
    #[test]
    fn a_configured_base_only_fills_a_detached_head() {
        let defaults = Defaults {
            base: Some("develop".into()),
            ..Defaults::default()
        };
        let on_a_branch = repo("api", Some("https://github.com/o/api.git"), Some("main"));
        let f = Form::new(on_a_branch, None, History::default(), &defaults);
        assert_eq!(f.input(Field::Base).unwrap().text(), "main");

        let detached = repo("api", Some("https://github.com/o/api.git"), None);
        let f = Form::new(detached, None, History::default(), &defaults);
        assert_eq!(f.input(Field::Base).unwrap().text(), "develop");
    }

    /// A provider that was used before but is not of a type this session wants
    /// must not be ticked -- history breaks ties, it does not invent them.
    #[test]
    fn history_only_breaks_ties_between_wanted_types() {
        let providers = Ok(vec![
            provider("claude-oauth", "claude-code-oauth"),
            provider("azure-pat", "azure-devops-pat"),
            provider("azure-pat-personal", "azure-devops-pat"),
        ]);
        let history = History {
            taken: vec![],
            // An Azure PAT, for a GitHub repository.
            providers: vec!["azure-pat".into()],
        };
        let f = Form::new(
            repo("api", Some("https://github.com/o/api.git"), Some("main")),
            Some(&providers),
            history,
            &Defaults::default(),
        );
        let ticked: Vec<&str> = f
            .providers()
            .iter()
            .filter(|c| c.selected)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            ticked,
            vec!["claude-oauth"],
            "no Azure PAT for a GitHub repo"
        );
    }

    /// A second session in the same repository is the normal case; with no task
    /// typed, both derive the repository's name, and the form must not have to be
    /// corrected by hand.
    #[test]
    fn a_derived_name_steps_around_the_ones_in_use() {
        let providers = Ok(vec![provider("claude-oauth", "claude-code-oauth")]);
        let history = History {
            taken: vec!["api".into()],
            providers: vec![],
        };
        let mut f = Form::new(
            repo("api", Some("https://github.com/o/api.git"), Some("main")),
            Some(&providers),
            history,
            &Defaults::default(),
        );
        assert_eq!(f.input(Field::Name).unwrap().text(), "api-2");

        // And it keeps stepping around them as the task is typed.
        for c in "api".chars() {
            f.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(f.input(Field::Name).unwrap().text(), "api-2");
    }

    #[test]
    fn space_toggles_a_provider() {
        let mut f = form();
        while f.field() != Field::Providers {
            f.on_key(key(KeyCode::Tab));
        }
        assert!(f.providers()[0].selected);
        f.on_key(key(KeyCode::Char(' ')));
        assert!(!f.providers()[0].selected);
        f.on_key(key(KeyCode::Down));
        f.on_key(key(KeyCode::Char(' ')));
        assert!(f.providers()[1].selected);
        assert_eq!(f.field(), Field::Providers, "j/k stay in the list");
    }

    #[test]
    fn a_late_provider_list_is_taken_but_never_overrides_a_choice() {
        let mut f = Form::new(
            repo("api", Some("https://github.com/o/api.git"), None),
            None,
            History::default(),
            &Defaults::default(),
        );
        assert!(f.providers().is_empty());
        assert!(f.providers_error().is_some());

        let list = Ok(vec![provider("claude-oauth", "claude-code-oauth")]);
        f.providers_arrived(&list);
        assert_eq!(f.providers().len(), 1);
        assert!(f.providers_error().is_none());

        // A second arrival must not reset what is now on screen.
        while f.field() != Field::Providers {
            f.on_key(key(KeyCode::Tab));
        }
        f.on_key(key(KeyCode::Char(' ')));
        assert!(!f.providers()[0].selected);
        f.providers_arrived(&list);
        assert!(!f.providers()[0].selected, "a refetch must not re-tick it");
    }

    #[test]
    fn submitting_produces_a_draft() {
        let mut f = form();
        for c in "fix the readme".chars() {
            f.on_key(key(KeyCode::Char(c)));
        }
        match f.on_key(key(KeyCode::Enter)) {
            Action::Submit(d) => {
                assert_eq!(d.name, "fix-readme");
                assert_eq!(d.repo, "https://github.com/o/api.git");
                assert_eq!(d.task, "fix the readme");
                assert_eq!(d.base.as_deref(), Some("main"));
                assert_eq!(d.policy, policy::DEFAULT_TEMPLATE);
                assert_eq!(d.providers, vec!["claude-oauth"]);
                assert!(d.start, "the point of the form is a working agent");
            }
            _ => panic!("expected a submit"),
        }
    }

    #[test]
    fn an_unusable_name_is_refused_with_the_cursor_on_it() {
        let mut f = form();
        while f.field() != Field::Name {
            f.on_key(key(KeyCode::Tab));
        }
        f.on_key(ctrl('u'));
        assert!(matches!(f.on_key(key(KeyCode::Enter)), Action::None));
        assert!(f.error().is_some());
        assert_eq!(f.field(), Field::Name);

        // A capital is equally invalid, and the message has to say why.
        f.on_key(key(KeyCode::Char('A')));
        f.on_key(key(KeyCode::Enter));
        assert!(f.error().unwrap().contains('A'), "got {:?}", f.error());
    }

    /// `git clone --branch` fails on a branch the remote does not have, so a
    /// local-only branch must not be sent as the base.
    #[test]
    fn a_branch_missing_from_the_remote_is_dropped_as_the_base() {
        let mut f = form();
        assert_eq!(f.input(Field::Base).unwrap().text(), "main");
        f.inspected(Facts {
            uncommitted: 2,
            unpushed: Some(1),
            base_on_remote: false,
            toolchains: Vec::new(),
        });
        assert_eq!(f.input(Field::Base).unwrap().text(), "");
        let draft = f.draft().unwrap();
        assert_eq!(draft.base, None, "the remote's default branch instead");
        assert_eq!(f.facts().unwrap().uncommitted, 2);
    }

    #[test]
    fn a_branch_present_on_the_remote_stays() {
        let mut f = form();
        f.inspected(Facts {
            uncommitted: 0,
            unpushed: Some(0),
            base_on_remote: true,
            toolchains: Vec::new(),
        });
        assert_eq!(f.draft().unwrap().base.as_deref(), Some("main"));
    }

    #[test]
    fn escape_from_the_form_goes_back_to_the_picker() {
        let mut f = form();
        assert!(matches!(f.on_key(key(KeyCode::Esc)), Action::Back));
    }

    /// Escape has to reach the form even while a text field has focus, or the
    /// only way out of a typo-ridden form is to create the session.
    #[test]
    fn escape_is_not_swallowed_by_a_text_field() {
        let mut f = form();
        for c in "typing".chars() {
            f.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(f.field(), Field::Task);
        assert!(matches!(f.on_key(key(KeyCode::Esc)), Action::Back));
    }
}
