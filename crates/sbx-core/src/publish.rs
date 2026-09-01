//! Publishing a session's work: push the branch, then open a pull request.
//!
//! Both happen *inside* the sandbox, which is the whole point -- the host never
//! holds the credential and never touches the working copy. One exec does the
//! lot, because exec on a sandbox is serialised and a push followed by a
//! separate REST call would queue behind itself.
//!
//! The two forges diverge only at the last step. GitHub has `gh` in the image
//! and it knows its own API. Azure DevOps has no equivalent short of the Azure
//! CLI plus its devops extension -- a Python runtime added to the image for a
//! single POST -- so its pull request is created with curl and jq, which are
//! already there.

use openshell_client::OpenShell;

use crate::forge::{self, Forge, Remote};
use crate::seed::sh_quote;
use crate::session::{REPO_PATH, Session};

/// Sentinels the script uses to report back.
///
/// Distinct from [`crate::pane`]'s display sigils: these are a machine-readable
/// result channel, not something a user ever sees, and they are matched at the
/// start of a line against output that also carries git's and curl's own
/// chatter on stderr.
const PUSHED: &str = "@@sbx-pushed@@";
const PR: &str = "@@sbx-pr@@ ";
const WARN: &str = "@@sbx-warn@@ ";
const FAILED: &str = "@@sbx-failed@@ ";

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Pull request title. Falls back to the session's task, then the branch.
    pub title: Option<String>,
    pub body: Option<String>,
    /// Branch to merge into. Defaults to the remote's default branch.
    pub target: Option<String>,
    /// Push only; do not open a pull request.
    pub no_pr: bool,
    pub draft: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub pushed: bool,
    /// Web URL of the pull request, when one was created or already existed.
    pub pull_request: Option<String>,
    /// Things that went wrong without failing the publish.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Client(#[from] openshell_client::Error),
    #[error(transparent)]
    Remote(#[from] forge::Error),
    #[error("publishing failed: {0}")]
    Script(String),
    #[error(
        "no credential reached the sandbox. Attach a provider when creating the \
         session:\n  sbx new ... --provider <name>\nwhere <name> is a provider \
         of profile `{profile}` (see: openshell provider list)"
    )]
    NoCredential { profile: &'static str },
    #[error(
        "the push was refused with 403. Two things cause that and the message \
         does not distinguish them:\n  1. the policy has no `git-receive-pack` \
         allow in its `{rule}` block -- `readonly-explore` deliberately has \
         none. Check with `sbx policy <session>`; the events pane shows the \
         denial.\n  2. the token lacks Code (Write) on that repository.\n\n\
         git said: {detail}"
    )]
    Denied { rule: &'static str, detail: String },
}

/// Push the work branch and open a pull request.
pub fn publish(
    client: &dyn OpenShell,
    session: &Session,
    opts: &Options,
) -> Result<Outcome, Error> {
    let remote = Remote::parse(&session.repo)?;
    let script = publish_script(session, &remote, opts);
    let out = client.exec(&session.sandbox, &["sh", "-c", &script])?;

    let mut outcome = Outcome::default();
    let mut failure = None;
    // Both streams: git writes progress to stderr, and a failure reported by
    // the script itself can land on either.
    for line in out.stdout.lines().chain(out.stderr.lines()) {
        let line = line.trim();
        if line == PUSHED {
            outcome.pushed = true;
        } else if let Some(url) = line.strip_prefix(PR) {
            outcome.pull_request = Some(url.trim().to_string());
        } else if let Some(w) = line.strip_prefix(WARN) {
            outcome.warnings.push(w.trim().to_string());
        } else if let Some(f) = line.strip_prefix(FAILED) {
            failure = Some(f.trim().to_string());
        }
    }

    if let Some(f) = failure {
        // Two failures are worth naming specially, because git's own wording
        // for each says nothing about the cause. A missing credential reads as
        // "could not read Username", and a policy denial reads as a transport
        // error -- the proxy refuses the CONNECT, so git sees a dead tunnel
        // rather than a 403.
        if f.contains("could not read Username") || f.contains("Authentication failed") {
            return Err(Error::NoCredential {
                profile: remote.forge.provider_profile(),
            });
        }
        // Measured against a real denial: a refused push surfaces as
        // `RPC failed; HTTP 403` and `curl 22 ... returned error: 403`, never as
        // the tidier strings the proxy uses elsewhere. Matching the status code
        // alone is deliberate -- the message covers both causes of a 403 rather
        // than guessing which one it was.
        if f.contains("403")
            || f.contains("CONNECT tunnel failed")
            || f.contains("not allowed by any policy")
        {
            return Err(Error::Denied {
                rule: remote.forge.policy_rule(),
                detail: f,
            });
        }
        return Err(Error::Script(f));
    }
    if !out.ok() && !outcome.pushed {
        return Err(Error::Script(format!(
            "exit {}: {}",
            out.exit_code,
            out.stderr.trim()
        )));
    }
    Ok(outcome)
}

/// The script [`publish`] runs. Separate so its shape can be asserted on
/// without a gateway.
fn publish_script(session: &Session, remote: &Remote, opts: &Options) -> String {
    let title = opts.title.clone().unwrap_or_else(|| {
        let task = session.task.lines().next().unwrap_or("").trim();
        if task.is_empty() {
            session.work_branch.clone()
        } else {
            task.to_string()
        }
    });
    let body = opts.body.clone().unwrap_or_else(|| {
        format!(
            "Opened by sbx from session `{}`.\n\n{}",
            session.name, session.task
        )
    });

    let target = match (&opts.target, &session.base_branch) {
        (Some(t), _) => format!("target={}\n", sh_quote(t)),
        (None, Some(b)) => format!("target={}\n", sh_quote(b)),
        // The clone recorded the remote's default branch; recover it rather
        // than assuming `main`, which is wrong for plenty of real repositories.
        (None, None) => String::from(
            "target=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null \
             | sed 's|^origin/||')\n",
        ),
    };

    format!(
        r#"set -u
cd {repo} 2>/dev/null || {{ printf '{failed}no repository at %s\n' {repo}; exit 1; }}
{prelude}
branch={branch}
{target}
if [ -z "${{target:-}}" ]; then
  printf '{failed}could not work out which branch to merge into; pass --target\n'
  exit 1
fi
if [ "$branch" = "$target" ]; then
  printf '{failed}the work branch and the target are both %s\n' "$branch"
  exit 1
fi

# Uncommitted work is not published. Saying so is better than pushing a branch
# that silently lacks the change the user just looked at in the diff pane.
if [ -n "$(git status --porcelain 2>/dev/null)" ]; then
  printf '{warn}uncommitted changes were not pushed; commit them and publish again\n'
fi

if ! git rev-parse --verify --quiet "refs/heads/$branch" >/dev/null 2>&1; then
  printf '{failed}no local branch %s\n' "$branch"
  exit 1
fi
# An empty branch is almost always a mistake rather than an intent.
ahead=$(git rev-list --count "origin/$target..$branch" 2>/dev/null || echo 0)
if [ "$ahead" = "0" ]; then
  printf '{failed}%s has no commits that %s does not already have\n' "$branch" "$target"
  exit 1
fi

push_out=$(gitc push --quiet --set-upstream origin "$branch" 2>&1) || {{
  printf '{failed}%s\n' "$(printf '%s' "$push_out" | tr '\n' ' ')"
  exit 1
}}
printf '{pushed}\n'

{pr}
"#,
        repo = sh_quote(REPO_PATH),
        prelude = forge::git_auth_prelude(remote.forge),
        branch = sh_quote(&session.work_branch),
        target = target,
        pushed = PUSHED,
        warn = WARN,
        failed = FAILED,
        pr = if opts.no_pr {
            String::new()
        } else {
            pull_request_script(remote, &title, &body, opts.draft)
        },
    )
}

/// The forge-specific half: opening the pull request.
fn pull_request_script(remote: &Remote, title: &str, body: &str, draft: bool) -> String {
    match remote.forge {
        Forge::GitHub => format!(
            r#"pr_out=$(gh pr create --title {title} --body {body} --base "$target" --head "$branch"{draft} 2>&1) || {{
  printf '{warn}pushed, but could not open a pull request: %s\n' "$(printf '%s' "$pr_out" | tr '\n' ' ')"
  exit 0
}}
printf '{pr}%s\n' "$(printf '%s' "$pr_out" | tail -n1)"
"#,
            title = sh_quote(title),
            body = sh_quote(body),
            draft = if draft { " --draft" } else { "" },
            warn = WARN,
            pr = PR,
        ),
        Forge::AzureDevOps => {
            // jq builds the body so a title containing a quote, newline or
            // backslash cannot break out of the JSON. Hand-rolled string
            // interpolation here would be an injection into the API call.
            let url = remote
                .pull_request_url()
                .unwrap_or_else(|| "MISSING".to_string());
            format!(
                r#"if [ -z "$auth_header" ]; then
  printf '{warn}pushed, but no credential to open a pull request with\n'
  exit 0
fi
pr_body=$(jq -n --arg src "refs/heads/$branch" --arg dst "refs/heads/$target" \
  --arg title {title} --arg desc {body} --argjson draft {draft} \
  '{{sourceRefName:$src, targetRefName:$dst, title:$title, description:$desc, isDraft:$draft}}')
pr_resp=$(curl -sS -m 60 -X POST -H "$auth_header" \
  -H 'Content-Type: application/json' -d "$pr_body" {url} 2>&1)
pr_id=$(printf '%s' "$pr_resp" | jq -r '.pullRequestId // empty' 2>/dev/null)
if [ -n "$pr_id" ]; then
  printf '{pr}{web}/pullrequest/%s\n' "$pr_id"
else
  # A pull request already open for this branch is a success, not a failure:
  # the push updated it. Azure DevOps reports it as TF401179.
  msg=$(printf '%s' "$pr_resp" | jq -r '.message // empty' 2>/dev/null)
  case "$msg" in
    *TF401179*|*'active pull request'*)
      printf '{pr}{web}/pullrequests?_a=mine\n' ;;
    *)
      printf '{warn}pushed, but could not open a pull request: %s\n' \
        "$(printf '%s' "${{msg:-$pr_resp}}" | tr '\n' ' ' | cut -c1-300)" ;;
  esac
fi
"#,
                title = sh_quote(title),
                body = sh_quote(body),
                draft = if draft { "true" } else { "false" },
                url = sh_quote(&url),
                web = format!(
                    "https://{}/{}/{}/_git/{}",
                    remote.host,
                    remote.org,
                    remote.project.as_deref().unwrap_or(""),
                    remote.repo
                ),
                warn = WARN,
                pr = PR,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(repo: &str) -> Session {
        let mut s = Session::new("add-auth".into(), repo.into(), "Add OAuth login".into());
        s.base_branch = None;
        s
    }

    const AZURE: &str = "https://tobiaswadseth0266@dev.azure.com/tobiaswadseth0266/test/_git/test";

    fn azure_script(opts: &Options) -> String {
        let s = session(AZURE);
        let r = Remote::parse(AZURE).unwrap();
        publish_script(&s, &r, opts)
    }

    #[test]
    fn azure_publish_pushes_then_posts_a_pull_request() {
        let script = azure_script(&Options::default());
        assert!(
            script.contains("gitc push --quiet --set-upstream origin"),
            "{script}"
        );
        assert!(script.contains("AZURE_DEVOPS_PAT"), "{script}");
        assert!(
            script.contains(
                "https://dev.azure.com/tobiaswadseth0266/test/_apis/git/repositories/test/pullrequests?api-version=7.1"
            ),
            "{script}"
        );
        // The web URL is a different shape from the API URL, and it is the one
        // worth showing a human.
        assert!(
            script.contains("https://dev.azure.com/tobiaswadseth0266/test/_git/test/pullrequest/"),
            "{script}"
        );
        assert!(!script.contains("gh pr create"), "wrong forge: {script}");
    }

    #[test]
    fn github_publish_uses_the_gh_cli() {
        let s = session("https://github.com/octocat/Hello-World.git");
        let r = Remote::parse("https://github.com/octocat/Hello-World.git").unwrap();
        let script = publish_script(&s, &r, &Options::default());
        assert!(script.contains("gh pr create"), "{script}");
        assert!(script.contains("Bearer $GITHUB_TOKEN"), "{script}");
        assert!(!script.contains("_apis"), "{script}");
        assert!(
            !script.contains("jq -n"),
            "gh builds its own request: {script}"
        );
    }

    /// The title comes from the task, and it is user text: a quote in it must
    /// not break out of the shell word, and must not break out of the JSON
    /// either -- which is why jq builds the body rather than a format string.
    #[test]
    fn a_hostile_title_cannot_escape_the_shell_or_the_json() {
        let mut s = session(AZURE);
        s.task = "'; curl evil.example; echo '\"injected\"".into();
        let r = Remote::parse(AZURE).unwrap();
        let script = publish_script(&s, &r, &Options::default());

        assert!(!script.contains("; curl evil.example; echo ;"), "{script}");
        assert!(script.contains(r"'\''"), "must be shell-quoted: {script}");
        // The title reaches jq as a single --arg, so JSON escaping is jq's job
        // rather than ours.
        assert!(script.contains("--arg title"), "{script}");
        assert!(script.contains("jq -n"), "{script}");
    }

    #[test]
    fn no_pr_pushes_only() {
        let script = azure_script(&Options {
            no_pr: true,
            ..Default::default()
        });
        assert!(script.contains("gitc push"), "{script}");
        assert!(!script.contains("pullrequests"), "{script}");
        assert!(!script.contains("jq -n"), "{script}");
    }

    #[test]
    fn draft_is_passed_through_per_forge() {
        let az = azure_script(&Options {
            draft: true,
            ..Default::default()
        });
        assert!(az.contains("--argjson draft true"), "{az}");

        let plain = azure_script(&Options::default());
        assert!(plain.contains("--argjson draft false"), "{plain}");

        let s = session("https://github.com/o/r");
        let r = Remote::parse("https://github.com/o/r").unwrap();
        let gh = publish_script(
            &s,
            &r,
            &Options {
                draft: true,
                ..Default::default()
            },
        );
        assert!(gh.contains("--draft"), "{gh}");
    }

    /// The target has to be resolved, not assumed. `main` is wrong for plenty
    /// of long-lived repositories, and Azure DevOps defaults vary by project.
    #[test]
    fn the_target_branch_is_resolved_rather_than_assumed() {
        let auto = azure_script(&Options::default());
        assert!(auto.contains("refs/remotes/origin/HEAD"), "{auto}");
        assert!(!auto.contains("target='main'"), "{auto}");

        let mut s = session(AZURE);
        s.base_branch = Some("develop".into());
        let r = Remote::parse(AZURE).unwrap();
        let pinned = publish_script(&s, &r, &Options::default());
        assert!(pinned.contains("target='develop'"), "{pinned}");

        // An explicit --target outranks the session's recorded base.
        let overridden = publish_script(
            &s,
            &r,
            &Options {
                target: Some("release/24".into()),
                ..Default::default()
            },
        );
        assert!(overridden.contains("target='release/24'"), "{overridden}");
    }

    /// Refusing beats pushing a branch that lacks the change the user just read
    /// in the diff pane, or opening a pull request with nothing in it.
    #[test]
    fn the_script_refuses_the_cases_that_would_publish_nothing() {
        let script = azure_script(&Options::default());
        assert!(script.contains("git status --porcelain"), "{script}");
        assert!(
            script.contains("uncommitted changes were not pushed"),
            "{script}"
        );
        assert!(script.contains("git rev-list --count"), "{script}");
        assert!(script.contains("has no commits that"), "{script}");
        // Publishing a branch into itself is always a mistake.
        assert!(script.contains(r#"[ "$branch" = "$target" ]"#), "{script}");
    }

    fn outcome_from(stdout: &str) -> Outcome {
        let mut o = Outcome::default();
        for line in stdout.lines() {
            let line = line.trim();
            if line == PUSHED {
                o.pushed = true;
            } else if let Some(u) = line.strip_prefix(PR) {
                o.pull_request = Some(u.trim().to_string());
            } else if let Some(w) = line.strip_prefix(WARN) {
                o.warnings.push(w.trim().to_string());
            }
        }
        o
    }

    /// The sentinels are matched against output that also carries git's and
    /// curl's chatter, so they have to survive being interleaved with it.
    #[test]
    fn the_result_is_parsed_out_of_mixed_output() {
        let stdout = format!(
            "Enumerating objects: 5, done.\n{PUSHED}\n\
             remote: Analyzing objects...\n\
             {WARN}uncommitted changes were not pushed\n\
             {PR}https://dev.azure.com/o/p/_git/r/pullrequest/42\n"
        );
        let o = outcome_from(&stdout);
        assert!(o.pushed);
        assert_eq!(
            o.pull_request.as_deref(),
            Some("https://dev.azure.com/o/p/_git/r/pullrequest/42")
        );
        assert_eq!(o.warnings, vec!["uncommitted changes were not pushed"]);
    }

    #[test]
    fn a_push_with_no_pull_request_is_still_a_push() {
        let o = outcome_from(&format!(
            "{PUSHED}\n{WARN}no credential to open a pull request with\n"
        ));
        assert!(o.pushed);
        assert_eq!(o.pull_request, None);
        assert_eq!(o.warnings.len(), 1);
    }

    /// An SSH remote cannot be published to, and the error has to say why
    /// before anything is attempted.
    #[test]
    fn an_ssh_remote_is_rejected_before_any_work() {
        let s = session("git@ssh.dev.azure.com:v3/org/proj/repo");
        let e = Remote::parse(&s.repo).unwrap_err();
        assert!(e.to_string().contains("HTTPS"), "{e}");
    }

    /// A policy denial reaches git as a transport error, because the proxy
    /// refuses the CONNECT and git never sees a 403. Left untranslated it looks
    /// like a network fault rather than the policy doing its job.
    #[test]
    fn a_policy_denial_names_the_rule_that_would_allow_it() {
        let e = Error::Denied {
            rule: Forge::AzureDevOps.policy_rule(),
            detail: "fatal: unable to access ...: CONNECT tunnel failed, response 403".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("azure_git"), "{msg}");
        assert!(msg.contains("git-receive-pack"), "{msg}");
        assert!(msg.contains("readonly-explore"), "{msg}");
        assert!(msg.contains("sbx policy"), "{msg}");
        // Both causes, because a 403 does not say which.
        assert!(msg.contains("Code (Write)"), "{msg}");
    }

    /// The wording git actually produces, captured from a push denied by
    /// `readonly-explore` against a real Azure DevOps repository. None of the
    /// proxy's tidier phrasings appear, which is why the matcher keys off the
    /// status code -- an earlier version looked for "403 Forbidden" and let the
    /// denial through as an untranslated script error.
    #[test]
    fn the_real_denial_wording_is_recognised() {
        let real = "error: RPC failed; HTTP 403 curl 22 The requested URL returned error: 403 \
                    send-pack: unexpected disconnect while reading sideband packet";
        assert!(real.contains("403"));
        assert!(!real.contains("CONNECT tunnel failed"));
        assert!(!real.contains("not allowed by any policy"));
        assert!(
            !real.contains("403 Forbidden"),
            "the old matcher missed this"
        );
    }

    /// git's own message for a missing credential says nothing actionable, so
    /// it is translated into the command that fixes it.
    #[test]
    fn a_missing_credential_names_the_provider_to_attach() {
        let e = Error::NoCredential {
            profile: Forge::AzureDevOps.provider_profile(),
        };
        let msg = e.to_string();
        assert!(msg.contains("azure-devops-pat"), "{msg}");
        assert!(msg.contains("--provider"), "{msg}");
        assert!(msg.contains("openshell provider list"), "{msg}");
    }
}
