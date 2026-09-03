//! The task inbox: what is assigned to you, in the trackers you use.
//!
//! Read **server-side over REST, with the credentials in the server's store**,
//! and that is a deliberate split rather than a duplication of the MCP servers
//! next door. REST is for what the *interface* shows: a list of tickets, on a
//! timer, rendered as rows. MCP is for what the *agent* gets: a tool it calls
//! when it decides to. They are different consumers with different failure
//! modes -- a list that cannot be fetched is a pane with a message in it, a tool
//! that cannot be reached is a session whose agent gives up on a step -- and
//! conflating them would make both worse.
//!
//! ## Why curl
//!
//! Three hosts, ordinary public certificates, JSON in and out. `curl` is
//! already on any machine that runs this and already how [`crate::publish`]
//! talks to Azure DevOps from inside a sandbox; the alternative is an HTTP
//! client, a TLS root store and a redirect policy pulled in for six requests.
//!
//! **The credential goes in on stdin, never in the argument list.** `curl -K -`
//! reads its configuration -- the url and the `Authorization` header
//! included -- from standard input, so a token never appears in `ps` output or
//! in the error text of a failed spawn. Same care as
//! [`crate::mcp::managed::start`], for the same reason.
//!
//! ## The round trip
//!
//! A session started from a ticket records which ticket, so publishing can
//! comment the pull request back onto it and move it along. That loop existed
//! as a personal skill; it is the thing an ADE should do with a button. Both
//! halves are best-effort and both say what happened: the branch is pushed and
//! the pull request is open either way, and losing a comment is not worth
//! failing a publish over.

use serde::{Deserialize, Serialize};

use crate::secrets;

/// A tracker this knows how to read.
// `TrackerKind` on the wire: `session::Kind` is already `Kind` in the one flat
// directory the bindings land in. Caught by the count check in
// `scripts/gen-bindings.sh`, which is what that check is for.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, rename = "TrackerKind"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    GitHub,
    AzureDevOps,
    Jira,
}

impl Kind {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "github" | "gh" => Ok(Kind::GitHub),
            "azure-devops" | "azure" | "ado" => Ok(Kind::AzureDevOps),
            "jira" | "atlassian" => Ok(Kind::Jira),
            other => Err(format!(
                "`{other}` is not a tracker; use github, azure-devops or jira"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::GitHub => "github",
            Kind::AzureDevOps => "azure-devops",
            Kind::Jira => "jira",
        }
    }
}

/// One configured tracker.
///
/// Validated when the config file is read, so a Jira entry with no site or an
/// Azure DevOps entry with no organisation fails against the line that wrote it
/// rather than against a 404 on a timer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub kind: Kind,
    /// What the inbox calls it. Defaults to the kind, which is right until
    /// somebody has two Jira sites.
    pub name: String,
    /// The name of the secret holding the credential. Never the credential:
    /// see [`crate::secrets`].
    pub secret: String,
    /// GitHub: `owner/name`, or `None` for everything assigned to you.
    pub repo: Option<String>,
    /// Azure DevOps organisation, and the project the query runs in.
    pub org: Option<String>,
    pub project: Option<String>,
    /// Jira site, `https://your-org.atlassian.net`, and the account the token
    /// belongs to -- Jira Cloud is Basic auth with the email as the username.
    pub site: Option<String>,
    pub email: Option<String>,
    /// The query to run: JQL for Jira, WIQL for Azure DevOps, a search
    /// qualifier string for GitHub. `None` means "assigned to me and not done",
    /// which is what an inbox is.
    pub query: Option<String>,
    /// What to move a ticket to when its session is published. `None` leaves it
    /// where it is.
    pub on_publish: Option<String>,
}

impl Source {
    /// What is missing, if anything.
    pub fn problem(&self) -> Option<String> {
        let missing = |what: &str| {
            Some(format!(
                "`{}` is a {} tracker with no {what}",
                self.name,
                self.kind.label()
            ))
        };
        match self.kind {
            Kind::Jira => {
                if self.site.is_none() {
                    return missing("site");
                }
                if self.email.is_none() {
                    // Jira Cloud's Basic auth is email + API token; a token
                    // alone authenticates as nobody.
                    return missing("email");
                }
                None
            }
            Kind::AzureDevOps => {
                if self.org.is_none() {
                    return missing("org");
                }
                if self.project.is_none() {
                    return missing("project");
                }
                None
            }
            Kind::GitHub => None,
        }
    }
}

/// One ticket, as every tracker's answer is flattened into.
///
/// Normalised here rather than in a client, because there are two clients and
/// three trackers: nine renderings, or one shape. The `id` is the tracker's own
/// and is what the write-back addresses; the `key` is what a person calls it.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Which configured tracker this came from, by name.
    pub tracker: String,
    pub kind: Kind,
    /// The tracker's own identifier: a work item id, an issue number, a Jira
    /// key. What a comment or a transition is addressed to.
    pub id: String,
    /// What a person calls it: `PROJ-123`, `#45`, `AB#1234`.
    pub key: String,
    pub title: String,
    /// Where to open it in a browser.
    pub url: String,
    /// Its state, in the tracker's own words -- `In Progress`, `Active`,
    /// `open`. Not mapped onto a scheme of ours: a status somebody configured
    /// is a fact about their process, and renaming it would lose it.
    pub status: String,
    /// Bug, Story, Task, whatever the tracker calls it. Empty when it has no
    /// such notion.
    pub item_type: String,
    /// The session name this ticket suggests: `proj-123-add-the-changelog`.
    /// Derived here so both front ends offer the same one.
    pub session_name: String,
    /// The branch it suggests, prefix included.
    pub branch: String,
    /// `owner/name`, for a GitHub issue. `None` for the other two, whose
    /// write-back is addressed by organisation and project from the config
    /// file. Carried because `/issues` spans repositories: a comment has to go
    /// to the one the issue is actually in, which the entry may not name.
    pub repo: Option<String>,
}

/// What a session remembers about the ticket it was started from.
///
/// On the session record, because the round trip happens at publish time --
/// minutes or days later, from a different client, possibly after the inbox has
/// moved on. Enough to address the write-back without asking the tracker
/// anything.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub tracker: String,
    pub kind: Kind,
    pub id: String,
    pub key: String,
    pub url: String,
    /// The GitHub repository the issue is in; see [`Task::repo`].
    #[serde(default)]
    pub repo: Option<String>,
}

impl From<&Task> for Ticket {
    fn from(t: &Task) -> Self {
        Ticket {
            tracker: t.tracker.clone(),
            kind: t.kind,
            id: t.id.clone(),
            key: t.key.clone(),
            url: t.url.clone(),
            repo: t.repo.clone(),
        }
    }
}

/// The inbox, and whatever could not be read.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inbox {
    pub tasks: Vec<Task>,
    /// One per tracker that failed, in words. A tracker that cannot be read is
    /// a row missing from a list, which is invisible -- so it is said out loud
    /// rather than left as an empty inbox.
    pub warnings: Vec<String>,
}

/// How long to give a tracker before giving up on it.
///
/// The inbox is polled, so a tracker that has gone away must not make the
/// window feel broken. Twenty seconds is generous for a search API and short
/// enough that three of them cannot stack into a minute.
const TIMEOUT: &str = "20";

/// Read every configured tracker.
///
/// Sequentially, because there are one or two of them and a thread per tracker
/// would buy milliseconds at the cost of ordering the result.
pub fn inbox(sources: &[Source], branch_prefix: &str) -> Inbox {
    let mut out = Inbox::default();
    for source in sources {
        if let Some(problem) = source.problem() {
            out.warnings.push(problem);
            continue;
        }
        match read(source, branch_prefix) {
            Ok(mut tasks) => out.tasks.append(&mut tasks),
            Err(e) => out.warnings.push(format!("{}: {e}", source.name)),
        }
    }
    out
}

fn read(source: &Source, prefix: &str) -> Result<Vec<Task>, String> {
    let token = secrets::get(&source.secret).ok_or_else(|| {
        format!(
            "no value stored for `{}`; set it from the integrations screen",
            source.secret
        )
    })?;
    match source.kind {
        Kind::GitHub => github(source, &token, prefix),
        Kind::AzureDevOps => azure(source, &token, prefix),
        Kind::Jira => jira(source, &token, prefix),
    }
}

// ---------------------------------------------------------------- GitHub

fn github(source: &Source, token: &str, prefix: &str) -> Result<Vec<Task>, String> {
    let auth = format!("Bearer {token}");
    // The repository's own issues when one is named, and everything assigned to
    // the token's owner otherwise -- which is what `/issues` means, and is the
    // whole inbox in one request.
    let url = match &source.repo {
        Some(repo) => format!(
            "https://api.github.com/repos/{repo}/issues?assignee=@me&state=open&per_page=50"
        ),
        None => "https://api.github.com/issues?filter=assigned&state=open&per_page=50".to_string(),
    };
    let body = get(&url, &auth, &["Accept: application/vnd.github+json"])?;
    parse_github(&body, source, prefix)
}

/// GitHub's answer, flattened. Separate from the request so every shape it
/// sends can be asserted on without a network -- which is the only way to have
/// any confidence in a reader of somebody else's JSON.
fn parse_github(
    body: &serde_json::Value,
    source: &Source,
    prefix: &str,
) -> Result<Vec<Task>, String> {
    let items = body.as_array().ok_or("github did not answer with a list")?;

    Ok(items
        .iter()
        // `/issues` returns pull requests too -- they are issues to GitHub --
        // and a pull request is not a task to start work on.
        .filter(|i| i.get("pull_request").is_none())
        .filter_map(|i| {
            let number = i.get("number")?.as_u64()?;
            let title = string(i, "title");
            let repo = i
                .get("repository")
                .map(|r| string(r, "full_name"))
                .filter(|s| !s.is_empty())
                .or_else(|| source.repo.clone())
                .unwrap_or_default();
            let key = format!("#{number}");
            Some(Task {
                tracker: source.name.clone(),
                kind: Kind::GitHub,
                id: number.to_string(),
                session_name: session_name(&key, &title),
                branch: branch(prefix, &key, &title),
                key,
                title,
                url: string(i, "html_url"),
                status: string(i, "state"),
                item_type: label_of(i).unwrap_or_default(),
                repo: (!repo.is_empty()).then_some(repo),
            })
        })
        .collect())
}

/// The first label, which is the closest GitHub has to a type.
fn label_of(issue: &serde_json::Value) -> Option<String> {
    let labels = issue.get("labels")?.as_array()?;
    labels.first().map(|l| string(l, "name"))
}

// ---------------------------------------------------------- Azure DevOps

fn azure(source: &Source, token: &str, prefix: &str) -> Result<Vec<Task>, String> {
    let org = source.org.as_deref().unwrap_or_default();
    let project = source.project.as_deref().unwrap_or_default();
    let auth = azure_auth(token);

    // Two requests, and there is no way around it: WIQL answers with ids only.
    let wiql = source.query.clone().unwrap_or_else(|| {
        "SELECT [System.Id] FROM WorkItems \
         WHERE [System.AssignedTo] = @Me \
         AND [System.State] NOT IN ('Closed', 'Done', 'Removed', 'Resolved') \
         ORDER BY [System.ChangedDate] DESC"
            .to_string()
    });
    let query_url =
        format!("https://dev.azure.com/{org}/{project}/_apis/wit/wiql?api-version=7.1&$top=50");
    let body = serde_json::json!({ "query": wiql });
    let answer = post(&query_url, &auth, &body.to_string(), JSON)?;

    let ids = parse_azure_ids(&answer);
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    // `fields` rather than the whole work item: a work item with its history is
    // tens of kilobytes and four of those fields are the whole row.
    let detail_url = format!(
        "https://dev.azure.com/{org}/{project}/_apis/wit/workitems?ids={}&fields=System.Id,System.Title,System.State,System.WorkItemType&api-version=7.1",
        ids.join(",")
    );
    let detail = get(&detail_url, &auth, &[])?;
    parse_azure(&detail, source, prefix)
}

/// The ids a WIQL query answered with. Its own function because the two-request
/// shape is the thing worth asserting: a query that matches nothing must not
/// produce a second request for zero ids, which Azure DevOps answers with a
/// 400.
fn parse_azure_ids(body: &serde_json::Value) -> Vec<String> {
    body.get("workItems")
        .and_then(|w| w.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("id").and_then(|v| v.as_u64()))
                .map(|id| id.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn parse_azure(
    detail: &serde_json::Value,
    source: &Source,
    prefix: &str,
) -> Result<Vec<Task>, String> {
    let org = source.org.as_deref().unwrap_or_default();
    let project = source.project.as_deref().unwrap_or_default();
    let items = detail
        .get("value")
        .and_then(|v| v.as_array())
        .ok_or("azure devops did not answer with work items")?;

    Ok(items
        .iter()
        .filter_map(|i| {
            let id = i.get("id")?.as_u64()?;
            let fields = i.get("fields")?;
            let title = string(fields, "System.Title");
            let key = format!("AB#{id}");
            Some(Task {
                tracker: source.name.clone(),
                kind: Kind::AzureDevOps,
                id: id.to_string(),
                session_name: session_name(&key, &title),
                branch: branch(prefix, &key, &title),
                key,
                title,
                // Built rather than read from `_links`, which needs `$expand`
                // and doubles the payload for a URL whose shape is fixed.
                url: format!("https://dev.azure.com/{org}/{project}/_workitems/edit/{id}"),
                status: string(fields, "System.State"),
                item_type: string(fields, "System.WorkItemType"),
                repo: None,
            })
        })
        .collect())
}

/// Azure DevOps PATs are HTTP Basic with the token as the *password* and an
/// empty username. A bearer token gets a 302 to a sign-in page rather than a
/// 401, which is a singularly unhelpful way to fail -- the same lesson
/// [`crate::forge`] records for git.
fn azure_auth(token: &str) -> String {
    format!(
        "Basic {}",
        crate::skills::base64(format!(":{token}").as_bytes())
    )
}

// ------------------------------------------------------------------ Jira

fn jira(source: &Source, token: &str, prefix: &str) -> Result<Vec<Task>, String> {
    let site = source
        .site
        .as_deref()
        .unwrap_or_default()
        .trim_end_matches('/');
    let email = source.email.as_deref().unwrap_or_default();
    let auth = format!(
        "Basic {}",
        crate::skills::base64(format!("{email}:{token}").as_bytes())
    );

    let jql = source.query.clone().unwrap_or_else(|| {
        // `statusCategory != Done` rather than a list of status names: every
        // Jira project renames its statuses and none of them rename the
        // categories.
        "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC".to_string()
    });
    // `/search/jql`, not `/search`: the older endpoint is deprecated on Jira
    // Cloud and answers 410 on newer sites.
    let url = format!(
        "{site}/rest/api/3/search/jql?jql={}&fields=summary,status,issuetype&maxResults=50",
        urlencode(&jql)
    );
    let body = get(&url, &auth, &["Accept: application/json"])?;
    parse_jira(&body, source, prefix)
}

fn parse_jira(
    body: &serde_json::Value,
    source: &Source,
    prefix: &str,
) -> Result<Vec<Task>, String> {
    let site = source
        .site
        .as_deref()
        .unwrap_or_default()
        .trim_end_matches('/');
    let issues = body
        .get("issues")
        .and_then(|i| i.as_array())
        .ok_or("jira did not answer with issues")?;

    Ok(issues
        .iter()
        .filter_map(|i| {
            let key = string(i, "key");
            if key.is_empty() {
                return None;
            }
            let fields = i.get("fields")?;
            let title = string(fields, "summary");
            Some(Task {
                tracker: source.name.clone(),
                kind: Kind::Jira,
                // The key *is* the id in Jira's API: every write-back path
                // takes an issue key or its numeric id interchangeably.
                id: key.clone(),
                session_name: session_name(&key, &title),
                branch: branch(prefix, &key, &title),
                url: format!("{site}/browse/{key}"),
                key,
                title,
                status: fields
                    .get("status")
                    .map(|s| string(s, "name"))
                    .unwrap_or_default(),
                item_type: fields
                    .get("issuetype")
                    .map(|t| string(t, "name"))
                    .unwrap_or_default(),
                repo: None,
            })
        })
        .collect())
}

// --------------------------------------------------------- naming things

/// The session name a ticket suggests: `proj-123-add-the-changelog`.
///
/// The key first, because that is what makes a session findable next to a
/// tracker, and the title after it for the sake of the person reading the list.
/// Truncated to what [`crate::session::validate_name`] accepts, at a dash, so
/// the result never ends mid-word or -- worse -- mid-dash.
pub fn session_name(key: &str, title: &str) -> String {
    let key = slug(key);
    let rest = slug(title);
    let joined = if rest.is_empty() {
        key.clone()
    } else {
        format!("{key}-{rest}")
    };
    // 40 is the session name limit; see `session::MAX_NAME`.
    let cut = truncate_at_dash(&joined, 40);
    if cut.is_empty() { key } else { cut }
}

/// The branch a ticket suggests, which is the convention this whole loop exists
/// to keep: `<prefix>/<KEY>-<description>`.
///
/// The key keeps its case here where the session name lowercases it: a branch
/// name is read by people and by the tracker's own commit hooks, and `PROJ-123`
/// is what both look for.
pub fn branch(prefix: &str, key: &str, title: &str) -> String {
    let key = key.trim().trim_start_matches('#').replace(' ', "");
    let rest = slug(title);
    let stem = if rest.is_empty() {
        key
    } else {
        format!("{key}-{}", truncate_at_dash(&rest, 40))
    };
    match prefix.trim().trim_matches('/') {
        "" => stem,
        prefix => format!("{prefix}/{stem}"),
    }
}

/// Lowercase, dashes, and nothing else. Deliberately not
/// [`crate::session::slugify`], which drops filler words to make a name out of
/// a *task description*; a ticket title is already a title and dropping "the"
/// from it would make it harder to recognise, not easier.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn truncate_at_dash(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let cut = &s[..max];
    match cut.rfind('-') {
        Some(i) if i > 0 => cut[..i].to_string(),
        _ => cut.trim_end_matches('-').to_string(),
    }
}

// ----------------------------------------------------------- the round trip

/// Comment the pull request onto the ticket, and move it if the tracker was
/// configured to.
///
/// Both halves are best-effort and each says what happened. The branch is
/// pushed and the pull request is open by the time this runs; losing a comment
/// is worth a warning and not worth failing a publish over -- and a transition
/// that does not exist is a configuration mistake to report, not a reason to
/// pretend the publish failed.
pub fn on_publish(sources: &[Source], ticket: &Ticket, pr_url: &str) -> Vec<String> {
    let Some(source) = sources.iter().find(|s| s.name == ticket.tracker) else {
        return vec![format!(
            "`{}` was started from {} in `{}`, which is no longer a configured tracker, so nothing was written back",
            ticket.key, ticket.key, ticket.tracker
        )];
    };
    let Some(token) = secrets::get(&source.secret) else {
        return vec![format!(
            "no value stored for `{}`, so {} was not updated",
            source.secret, ticket.key
        )];
    };

    let mut warnings = Vec::new();
    if let Err(e) = comment(source, &token, ticket, pr_url) {
        warnings.push(format!("could not comment on {}: {e}", ticket.key));
    }
    if let Some(target) = &source.on_publish
        && let Err(e) = transition(source, &token, ticket, target)
    {
        warnings.push(format!("could not move {} to `{target}`: {e}", ticket.key));
    }
    warnings
}

fn comment(source: &Source, token: &str, ticket: &Ticket, pr_url: &str) -> Result<(), String> {
    let text = format!("Pull request: {pr_url}");
    match ticket.kind {
        Kind::GitHub => {
            // The issue's own repository first: `/issues` spans several, and
            // the entry may name none.
            let repo = ticket
                .repo
                .clone()
                .or_else(|| source.repo.clone())
                .ok_or("which github repository? the issue's own was not recorded")?;
            let url = format!(
                "https://api.github.com/repos/{repo}/issues/{}/comments",
                ticket.id
            );
            let body = serde_json::json!({ "body": text });
            post(&url, &format!("Bearer {token}"), &body.to_string(), JSON).map(|_| ())
        }
        Kind::AzureDevOps => {
            let org = source.org.as_deref().unwrap_or_default();
            let project = source.project.as_deref().unwrap_or_default();
            // The comments API is still preview-versioned; 7.1-preview.3 is
            // what answers on dev.azure.com.
            let url = format!(
                "https://dev.azure.com/{org}/{project}/_apis/wit/workItems/{}/comments?api-version=7.1-preview.3",
                ticket.id
            );
            let body = serde_json::json!({ "text": text });
            post(&url, &azure_auth(token), &body.to_string(), JSON).map(|_| ())
        }
        Kind::Jira => {
            let site = source
                .site
                .as_deref()
                .unwrap_or_default()
                .trim_end_matches('/');
            let email = source.email.as_deref().unwrap_or_default();
            let url = format!("{site}/rest/api/3/issue/{}/comment", ticket.id);
            // Atlassian Document Format: a Jira Cloud comment body is a
            // document, not a string, and a string is rejected with a 400.
            let body = serde_json::json!({
                "body": {
                    "type": "doc",
                    "version": 1,
                    "content": [{
                        "type": "paragraph",
                        "content": [{ "type": "text", "text": text }]
                    }]
                }
            });
            post(
                &url,
                &format!(
                    "Basic {}",
                    crate::skills::base64(format!("{email}:{token}").as_bytes())
                ),
                &body.to_string(),
                JSON,
            )
            .map(|_| ())
        }
    }
}

fn transition(source: &Source, token: &str, ticket: &Ticket, target: &str) -> Result<(), String> {
    match ticket.kind {
        Kind::Jira => {
            let site = source.site.as_deref().unwrap_or_default().trim_end_matches('/');
            let email = source.email.as_deref().unwrap_or_default();
            let auth = format!(
                "Basic {}",
                crate::skills::base64(format!("{email}:{token}").as_bytes())
            );
            // Jira moves an issue by *transition id*, and which transitions
            // exist depends on the workflow and the issue's current status. So
            // the target is matched by name against what this issue can
            // actually do, and a name that is not among them says which are --
            // the alternative is a 400 that names neither.
            let url = format!("{site}/rest/api/3/issue/{}/transitions", ticket.id);
            let body = get(&url, &auth, &["Accept: application/json"])?;
            let transitions = body
                .get("transitions")
                .and_then(|t| t.as_array())
                .ok_or("jira did not answer with transitions")?;
            let found = transitions.iter().find(|t| {
                let name = string(t, "name");
                let to = t.get("to").map(|to| string(to, "name")).unwrap_or_default();
                name.eq_ignore_ascii_case(target) || to.eq_ignore_ascii_case(target)
            });
            let id = match found {
                Some(t) => string(t, "id"),
                None => {
                    let available: Vec<String> = transitions
                        .iter()
                        .map(|t| {
                            t.get("to")
                                .map(|to| string(to, "name"))
                                .unwrap_or_else(|| string(t, "name"))
                        })
                        .collect();
                    return Err(format!(
                        "no transition to `{target}` from `{}`; it can go to: {}",
                        ticket.key,
                        available.join(", ")
                    ));
                }
            };
            let body = serde_json::json!({ "transition": { "id": id } });
            post(&url, &auth, &body.to_string(), JSON).map(|_| ())
        }
        Kind::AzureDevOps => {
            let org = source.org.as_deref().unwrap_or_default();
            let project = source.project.as_deref().unwrap_or_default();
            let url = format!(
                "https://dev.azure.com/{org}/{project}/_apis/wit/workitems/{}?api-version=7.1",
                ticket.id
            );
            let body = serde_json::json!([{
                "op": "add",
                "path": "/fields/System.State",
                "value": target,
            }]);
            // A work item is edited with a JSON *patch*, and the content type
            // is what tells Azure DevOps that: `application/json` on this body
            // is a 400.
            patch(&url, &azure_auth(token), &body.to_string(), JSON_PATCH).map(|_| ())
        }
        // GitHub has no status between open and closed, and closing an issue
        // because a pull request exists is a decision for the person merging
        // it. Said rather than silently doing nothing.
        Kind::GitHub => Err(
            "github issues have no status to move to; a pull request that says `Fixes #n` closes it on merge"
                .into(),
        ),
    }
}

// -------------------------------------------------------------------- curl

const JSON: &str = "application/json";
const JSON_PATCH: &str = "application/json-patch+json";

fn get(url: &str, auth: &str, headers: &[&str]) -> Result<serde_json::Value, String> {
    curl(url, auth, headers, None)
}

fn post(
    url: &str,
    auth: &str,
    body: &str,
    content_type: &str,
) -> Result<serde_json::Value, String> {
    curl(url, auth, &[], Some(("POST", body, content_type)))
}

fn patch(
    url: &str,
    auth: &str,
    body: &str,
    content_type: &str,
) -> Result<serde_json::Value, String> {
    curl(url, auth, &[], Some(("PATCH", body, content_type)))
}

/// One request, with the credential on stdin.
///
/// `-K -` makes curl read its configuration from standard input, which is how
/// the url and the `Authorization` header stay out of the argument list -- and
/// so out of `ps`, out of a failed spawn's error text, and out of anything that
/// logs a command line. The body goes the same way, since a body can carry a
/// credential too.
///
/// The status code is asked for separately and printed after the body, because
/// curl's exit code is about the transport: a 401 is a request that worked and
/// an answer that has to be read as one.
fn curl(
    url: &str,
    auth: &str,
    headers: &[&str],
    body: Option<(&str, &str, &str)>,
) -> Result<serde_json::Value, String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let mut config = String::new();
    config.push_str(&format!("url = {}\n", quote(url)));
    config.push_str(&format!("header = {}\n", quote(&auth_header(auth))));
    // Every one of these APIs answers JSON and some of them need to be told.
    config.push_str(&format!("header = {}\n", quote(&format!("Accept: {JSON}"))));
    // GitHub refuses a request with no user agent, with a 403 that says so in
    // prose.
    config.push_str("user-agent = \"sbx\"\n");
    for header in headers {
        config.push_str(&format!("header = {}\n", quote(header)));
    }
    config.push_str("silent\nshow-error\nlocation\n");
    config.push_str(&format!("max-time = {TIMEOUT}\n"));
    // The status code, after the body, on its own line.
    config.push_str("write-out = \"\\n%{http_code}\"\n");
    if let Some((method, body, content_type)) = body {
        config.push_str(&format!("request = {}\n", quote(method)));
        config.push_str(&format!(
            "header = {}\n",
            quote(&format!("Content-Type: {content_type}"))
        ));
        config.push_str(&format!("data-raw = {}\n", quote(body)));
    }

    let mut child = Command::new("curl")
        .arg("-K")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run curl: {e}"))?;
    child
        .stdin
        .as_mut()
        .ok_or("curl took no input")?
        .write_all(config.as_bytes())
        .map_err(|e| format!("could not write to curl: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("curl did not finish: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "the request failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    interpret(&String::from_utf8_lossy(&out.stdout))
}

/// Split the body from the status line curl appended, and read one as the other
/// asks for.
///
/// Separate from the request so every status the trackers answer with can be
/// asserted on without a network.
fn interpret(stdout: &str) -> Result<serde_json::Value, String> {
    let (body, code) = stdout
        .rsplit_once('\n')
        .ok_or("the request produced nothing at all")?;
    let code: u16 = code.trim().parse().unwrap_or(0);
    let body = body.trim();

    if (200..300).contains(&code) {
        if body.is_empty() {
            // A comment posts and answers 201 with a body some of the time and
            // 204 with none the rest; neither is a failure.
            return Ok(serde_json::Value::Null);
        }
        return serde_json::from_str(body)
            .map_err(|e| format!("the answer was not json: {e}: {}", first_line(body)));
    }

    // Each of the three says what is wrong in a different field, and all three
    // are worth quoting rather than replacing with "the request failed": a
    // wrong project name and an expired token are the same status code.
    let said = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            for key in ["message", "errorMessages", "error_description", "detail"] {
                if let Some(found) = v.get(key) {
                    return Some(match found {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    });
                }
            }
            None
        })
        .unwrap_or_else(|| first_line(body));

    Err(match code {
        401 | 403 => format!("{code}: {said} (is the stored credential still valid?)"),
        404 => format!("404: {said} (check the org, project or site in the config file)"),
        0 => format!("no status came back: {said}"),
        _ => format!("{code}: {said}"),
    })
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(200).collect()
}

/// The credential as a header line.
///
/// Every caller above builds the *value* -- `Bearer x`, `Basic y` -- and the
/// name is added here, so nothing that reaches this file can name a header of
/// its own. It was missing, briefly, and the failure is worth recording: curl
/// takes a `-H` string with no colon in it as an instruction to *remove* a
/// header, so the request went out with no `Authorization` at all and the
/// trackers answered 401. Nothing in the unit tests could see it, which is why
/// there is a test that sends a request to a listener and reads the headers.
fn auth_header(auth: &str) -> String {
    format!("Authorization: {auth}")
}

/// A curl config value: double quotes, with backslashes and quotes escaped.
///
/// Written out rather than assumed, because everything interpolated above is
/// attacker-influenced in the general case -- a ticket title, a JQL string, a
/// token -- and an unescaped quote would end the value and start a new
/// configuration line, which in a curl config file can name a file to write.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            // A newline would end the line whatever the quoting; there is no
            // escape for one in a curl config, so it is dropped. None of the
            // values here legitimately contain one.
            '\n' | '\r' => out.push(' '),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Percent-encode everything that is not unreserved.
///
/// JQL is full of spaces, quotes, equals signs and parentheses, and it goes in
/// a query string. A small table rather than a dependency: this is the only
/// place in the crate that needs one.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn string(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(kind: Kind) -> Source {
        Source {
            kind,
            name: kind.label().to_string(),
            secret: "TOKEN".into(),
            repo: None,
            org: Some("inetse".into()),
            project: Some("inet".into()),
            site: Some("https://example.atlassian.net".into()),
            email: Some("you@example.com".into()),
            query: None,
            on_publish: None,
        }
    }

    /// The shape `GET /issues?filter=assigned` answers with, trimmed to the
    /// fields this reads -- including a pull request, which GitHub returns from
    /// the same endpoint because a pull request *is* an issue to it.
    const GITHUB: &str = r#"[
      {
        "number": 45,
        "title": "Readme says the wrong port",
        "html_url": "https://github.com/o/r/issues/45",
        "state": "open",
        "labels": [{ "name": "bug" }, { "name": "docs" }],
        "repository": { "full_name": "o/r" }
      },
      {
        "number": 46,
        "title": "Bump the lockfile",
        "html_url": "https://github.com/o/r/pull/46",
        "state": "open",
        "labels": [],
        "repository": { "full_name": "o/r" },
        "pull_request": { "url": "https://api.github.com/repos/o/r/pulls/46" }
      }
    ]"#;

    #[test]
    fn a_github_issue_becomes_a_task_and_a_pull_request_does_not() {
        let tasks = parse_github(
            &serde_json::from_str(GITHUB).unwrap(),
            &source(Kind::GitHub),
            "tobias",
        )
        .unwrap();

        assert_eq!(tasks.len(), 1, "a pull request is not a task: {tasks:?}");
        let t = &tasks[0];
        assert_eq!(t.key, "#45");
        assert_eq!(t.id, "45");
        assert_eq!(t.title, "Readme says the wrong port");
        assert_eq!(t.status, "open");
        assert_eq!(t.item_type, "bug", "the first label is the closest thing");
        assert_eq!(t.repo.as_deref(), Some("o/r"), "a comment needs it");
        assert_eq!(t.session_name, "45-readme-says-the-wrong-port");
        assert_eq!(t.branch, "tobias/45-readme-says-the-wrong-port");
    }

    /// Two requests, because WIQL answers with ids only.
    #[test]
    fn a_wiql_answer_of_ids_becomes_one_detail_request_or_none() {
        let ids = parse_azure_ids(
            &serde_json::from_str(r#"{ "workItems": [{ "id": 1234 }, { "id": 99 }] }"#).unwrap(),
        );
        assert_eq!(ids, ["1234", "99"]);

        // A query that matches nothing: asking for zero ids is a 400, so the
        // second request has to be skipped rather than built.
        assert!(parse_azure_ids(&serde_json::json!({ "workItems": [] })).is_empty());
        assert!(parse_azure_ids(&serde_json::json!({})).is_empty());
    }

    const AZURE: &str = r#"{
      "count": 1,
      "value": [
        {
          "id": 1234,
          "fields": {
            "System.Id": 1234,
            "System.Title": "Order backfill throws on empty batch",
            "System.State": "Active",
            "System.WorkItemType": "Bug"
          }
        }
      ]
    }"#;

    #[test]
    fn a_work_item_becomes_a_task_with_a_url_that_was_not_in_the_answer() {
        let tasks = parse_azure(
            &serde_json::from_str(AZURE).unwrap(),
            &source(Kind::AzureDevOps),
            "tobias",
        )
        .unwrap();

        assert_eq!(tasks.len(), 1);
        let t = &tasks[0];
        assert_eq!(t.key, "AB#1234");
        assert_eq!(t.id, "1234");
        assert_eq!(t.status, "Active");
        assert_eq!(t.item_type, "Bug");
        // Built rather than read: `_links` needs `$expand` and doubles the
        // payload for a url whose shape is fixed.
        assert_eq!(
            t.url,
            "https://dev.azure.com/inetse/inet/_workitems/edit/1234"
        );
        assert_eq!(
            t.branch,
            "tobias/AB#1234-order-backfill-throws-on-empty-batch"
        );
    }

    const JIRA: &str = r#"{
      "issues": [
        {
          "key": "PROJ-123",
          "fields": {
            "summary": "Add the changelog",
            "status": { "name": "In Progress", "statusCategory": { "key": "indeterminate" } },
            "issuetype": { "name": "Story" }
          }
        }
      ]
    }"#;

    #[test]
    fn a_jira_issue_keeps_its_own_status_words() {
        let tasks = parse_jira(
            &serde_json::from_str(JIRA).unwrap(),
            &source(Kind::Jira),
            "tobias",
        )
        .unwrap();

        assert_eq!(tasks.len(), 1);
        let t = &tasks[0];
        assert_eq!(t.key, "PROJ-123");
        assert_eq!(t.id, "PROJ-123", "jira addresses writes by key");
        // Not mapped onto a scheme of ours: a renamed status is a fact about
        // somebody's process.
        assert_eq!(t.status, "In Progress");
        assert_eq!(t.item_type, "Story");
        assert_eq!(t.url, "https://example.atlassian.net/browse/PROJ-123");
        assert_eq!(t.session_name, "proj-123-add-the-changelog");
        assert_eq!(t.branch, "tobias/PROJ-123-add-the-changelog");
    }

    /// The convention this whole loop exists to keep. The key keeps its case in
    /// a branch -- commit hooks and people both look for `PROJ-123` -- and
    /// loses it in a session name, which has to satisfy `validate_name`.
    #[test]
    fn a_ticket_names_its_session_and_its_branch() {
        let long = "Make the nightly reconciliation job cope with a partial batch from upstream";
        let name = session_name("PROJ-123", long);
        assert!(name.len() <= 40, "{name} is {}", name.len());
        assert!(
            crate::session::validate_name(&name).is_ok(),
            "{name}: {:?}",
            crate::session::validate_name(&name)
        );
        assert!(!name.ends_with('-'), "{name}");
        assert!(name.starts_with("proj-123-"), "{name}");

        assert_eq!(branch("", "PROJ-1", "Fix it"), "PROJ-1-fix-it");
        assert_eq!(
            branch("/tobias/", "PROJ-1", "Fix it"),
            "tobias/PROJ-1-fix-it"
        );
        // A title that survives nothing still gives a usable name.
        assert_eq!(session_name("PROJ-9", "!!!"), "proj-9");
        assert_eq!(branch("t", "PROJ-9", "  "), "t/PROJ-9");
    }

    /// A status code is not a transport failure, and each tracker says what is
    /// wrong in a different field. All three are quoted rather than replaced,
    /// because an expired token and a misspelled project are the same code.
    #[test]
    fn a_failed_request_is_read_rather_than_summarised() {
        assert_eq!(
            interpret("{\"ok\":true}\n200").unwrap(),
            serde_json::json!({ "ok": true })
        );
        // 204, which a posted comment answers with.
        assert_eq!(interpret("\n204").unwrap(), serde_json::Value::Null);

        let e = interpret("{\"message\":\"Bad credentials\"}\n401").unwrap_err();
        assert!(
            e.contains("Bad credentials") && e.contains("still valid"),
            "{e}"
        );

        // Jira's shape.
        let e = interpret("{\"errorMessages\":[\"Issue does not exist\"]}\n404").unwrap_err();
        assert!(e.contains("Issue does not exist"), "{e}");
        assert!(e.contains("config file"), "{e}");

        // Azure DevOps sends HTML for some failures, and quoting the first
        // line of it beats saying nothing.
        let e = interpret("<html><head><title>Sign in</title>\n203").unwrap_err();
        assert!(e.contains("203") || e.contains("html"), "{e}");
    }

    /// Everything interpolated into a curl config is attacker-influenced in the
    /// general case -- a ticket title, a JQL string, a token -- and an
    /// unescaped quote would end the value and start a new configuration line,
    /// which in a curl config can name a file to write.
    #[test]
    fn a_config_value_cannot_start_a_new_line() {
        let hostile = "a\" \noutput = \"/tmp/owned\" \"b";
        let quoted = quote(hostile);
        assert!(quoted.starts_with('"') && quoted.ends_with('"'));
        assert!(!quoted[1..quoted.len() - 1].contains('\n'), "{quoted}");
        assert_eq!(quoted.matches("\\\"").count(), 4, "{quoted}");
        // And the one that would escape the escaping.
        assert_eq!(quote(r"a\b"), r#""a\\b""#);
    }

    /// JQL is spaces, quotes, equals signs and parentheses, and it goes in a
    /// query string.
    #[test]
    fn jql_survives_being_a_query_parameter() {
        assert_eq!(
            urlencode("assignee = currentUser() AND status != \"Done\""),
            "assignee%20%3D%20currentUser%28%29%20AND%20status%20%21%3D%20%22Done%22"
        );
        assert_eq!(urlencode("plain-Text_1.0~"), "plain-Text_1.0~");
    }

    /// A tracker entry that cannot work says so against the config file rather
    /// than against a 404 on a timer.
    #[test]
    fn an_incomplete_tracker_entry_says_what_is_missing() {
        let mut jira = source(Kind::Jira);
        assert_eq!(jira.problem(), None);
        jira.email = None;
        assert!(jira.problem().unwrap().contains("no email"));
        jira.site = None;
        assert!(jira.problem().unwrap().contains("no site"));

        let mut azure = source(Kind::AzureDevOps);
        assert_eq!(azure.problem(), None);
        azure.project = None;
        assert!(azure.problem().unwrap().contains("no project"));

        // GitHub needs nothing beyond a token: with no repo it reads
        // everything assigned to whoever the token belongs to.
        let github = source(Kind::GitHub);
        assert_eq!(github.problem(), None);
    }

    #[test]
    fn a_tracker_kind_is_read_generously_and_refused_clearly() {
        assert_eq!(Kind::parse("GitHub").unwrap(), Kind::GitHub);
        assert_eq!(Kind::parse("azure_devops").unwrap(), Kind::AzureDevOps);
        assert_eq!(Kind::parse(" ado ").unwrap(), Kind::AzureDevOps);
        assert_eq!(Kind::parse("jira").unwrap(), Kind::Jira);
        let e = Kind::parse("linear").unwrap_err();
        assert!(e.contains("github") && e.contains("jira"), "{e}");
    }
}
