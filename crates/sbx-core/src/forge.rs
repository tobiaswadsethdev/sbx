//! Which git host a session is working against, and what that implies.
//!
//! Two things differ between hosts and nothing else does: the policy rules that
//! let git reach them, and how a pull request is opened. Both are derived from
//! the repo URL rather than configured, because the URL is the one thing a
//! session always has and cannot be wrong about.
//!
//! Azure DevOps is the awkward one. `dev.azure.com` serves the git endpoints
//! and the REST API from a single host, its paths carry a *project* between the
//! organisation and the repository, and the URL the web UI hands you has the
//! organisation in the userinfo position -- which changes how git authenticates.
//! See [`Remote::parse`].

/// A git host `sbx` knows how to publish to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forge {
    GitHub,
    AzureDevOps,
}

impl Forge {
    /// The policy rule that grants this forge's git traffic, as named in the
    /// shipped templates. Shown when a publish is denied, so the message can
    /// name the block to look at rather than saying "check your policy".
    pub fn policy_rule(self) -> &'static str {
        match self {
            Forge::GitHub => "github_git",
            Forge::AzureDevOps => "azure_git",
        }
    }

    /// The provider profile that carries a credential for it.
    pub fn provider_profile(self) -> &'static str {
        match self {
            Forge::GitHub => "github",
            Forge::AzureDevOps => "azure-devops-pat",
        }
    }

    /// Environment variable the provider leaves a credential *reference* in.
    ///
    /// Not the credential. The gateway sets this to a placeholder of the form
    /// `openshell:resolve:env:v<id>_<NAME>` and substitutes the real secret
    /// into any outgoing header containing it -- including inside the base64 of
    /// a Basic credential, which is what makes [`Self::auth_header_expr`] work.
    /// So the value is safe to write into a git config, and the secret never
    /// exists inside the sandbox at all.
    pub fn credential_env(self) -> &'static str {
        match self {
            // First of the profile's env_vars; see providers/azure-devops-pat.yaml.
            Forge::AzureDevOps => "AZURE_DEVOPS_PAT",
            Forge::GitHub => "GITHUB_TOKEN",
        }
    }

    /// Shell expression evaluating to an `Authorization` header value.
    ///
    /// Azure DevOps PATs are HTTP Basic with the token as the *password* and an
    /// empty username -- `base64(":" + pat)`. Sending one as a bearer token
    /// gets a 302 to an Entra sign-in page rather than a 401, which is a
    /// singularly unhelpful way to fail. GitHub tokens are bearer, matching the
    /// builtin `github` provider profile's `auth_style`.
    pub fn auth_header_expr(self) -> String {
        let var = self.credential_env();
        match self {
            Forge::AzureDevOps => {
                format!(r#"Authorization: Basic $(printf ':%s' "${var}" | base64 -w0)"#)
            }
            Forge::GitHub => format!(r#"Authorization: Bearer ${var}"#),
        }
    }
}

/// Shell that leaves a ready-to-use `git -c` argument in `$git_auth`, and the
/// bare header in `$auth_header` for curl.
///
/// Both are empty when the credential variable is unset, and every git call
/// below is written to work either way -- because a public repository needs no
/// credential and requiring a provider to clone one would be a regression.
///
/// `http.extraHeader` rather than a credential helper or a URL with userinfo.
/// A helper would have to be written and installed in the image; userinfo makes
/// git demand a password for that username *before* it sends anything, so it
/// fails with "could not read Username" while the gateway waits to authenticate
/// a request git never makes.
pub fn git_auth_prelude(forge: Forge) -> String {
    format!(
        r#"git_auth=''
auth_header=''
if [ -n "${{{var}:-}}" ]; then
  auth_header="{header}"
  git_auth="http.extraHeader=$auth_header"
fi
# Wrapper so every later call is credential-agnostic: with no token this is a
# plain `git`, and the -c argument is never passed as an empty string (which
# git rejects rather than ignores).
gitc() {{
  if [ -n "$git_auth" ]; then git -c "$git_auth" "$@"; else git "$@"; fi
}}
"#,
        var = forge.credential_env(),
        header = forge.auth_header_expr(),
    )
}

impl std::fmt::Display for Forge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Forge::GitHub => "github",
            Forge::AzureDevOps => "azure-devops",
        })
    }
}

/// A parsed remote: enough to clone it, push to it, and open a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub forge: Forge,
    pub host: String,
    /// GitHub owner, or Azure DevOps organisation.
    pub org: String,
    /// Azure DevOps project. GitHub has no equivalent level.
    pub project: Option<String>,
    pub repo: String,
    /// The URL to hand to `git clone`, with any userinfo removed.
    pub clone_url: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error(
        "`{0}` is an SSH remote; sbx clones over HTTPS so the sandbox needs no key.\n\
         Use the HTTPS URL for the same repository instead."
    )]
    Ssh(String),
    #[error("could not tell which git host `{0}` is; sbx knows github.com and dev.azure.com")]
    UnknownHost(String),
    #[error("`{url}` is missing the {missing} part of a {forge} repository path")]
    Incomplete {
        url: String,
        missing: &'static str,
        forge: Forge,
    },
}

impl Remote {
    /// Parse a repository URL.
    ///
    /// Accepts the forms each host's UI actually hands out, which for Azure
    /// DevOps includes `https://org@dev.azure.com/org/project/_git/repo` -- the
    /// default offered by the "Clone" button. The userinfo is stripped rather
    /// than kept: with a username in the URL git demands a password for it
    /// before sending anything, and fails with "could not read Username" even
    /// though the gateway would have injected a working credential into the
    /// request. Without it git sends the request unauthenticated, the gateway
    /// adds the header, and there is nothing to prompt for.
    pub fn parse(url: &str) -> Result<Self, Error> {
        let url = url.trim();

        // scp-like SSH remotes have no scheme and a colon before the path.
        if url.starts_with("git@") || url.starts_with("ssh://") {
            return Err(Error::Ssh(url.to_string()));
        }

        let (scheme, rest) = url
            .split_once("://")
            .map_or(("https", url), |(s, r)| (s, r));
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        // Drop userinfo, and any port, to get the bare host.
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = host_port.split(':').next().unwrap_or(host_port);

        let segments: Vec<&str> = path
            .trim_end_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        let clone_url = format!("{scheme}://{host_port}/{}", segments.join("/"));
        let strip_git = |s: &str| s.trim_end_matches(".git").to_string();

        // Legacy Azure DevOps organisations still answer on
        // `<org>.visualstudio.com`, and plenty of long-lived remotes use it.
        let is_azure = host == "dev.azure.com"
            || host.ends_with(".visualstudio.com")
            || host == "ssh.dev.azure.com";

        if is_azure {
            // `_git` separates the project from the repository, and is the only
            // reliable landmark: an organisation, a project or a repository can
            // all contain characters that make positional parsing wrong.
            let split = segments.iter().position(|s| *s == "_git");
            let Some(i) = split else {
                return Err(Error::Incomplete {
                    url: url.to_string(),
                    missing: "`_git/`",
                    forge: Forge::AzureDevOps,
                });
            };
            let repo = match segments.get(i + 1) {
                Some(r) => strip_git(r),
                None => {
                    return Err(Error::Incomplete {
                        url: url.to_string(),
                        missing: "repository",
                        forge: Forge::AzureDevOps,
                    });
                }
            };
            // On `<org>.visualstudio.com` the organisation is in the hostname
            // and absent from the path, so the path segments before `_git` are
            // the project alone.
            let before: Vec<&str> = segments[..i].to_vec();
            let (org, project) = if host.ends_with(".visualstudio.com") {
                let org = host.trim_end_matches(".visualstudio.com").to_string();
                (org, before.first().map(|s| (*s).to_string()))
            } else {
                match before.split_first() {
                    Some((org, tail)) => {
                        ((*org).to_string(), tail.first().map(|s| (*s).to_string()))
                    }
                    None => {
                        return Err(Error::Incomplete {
                            url: url.to_string(),
                            missing: "organisation",
                            forge: Forge::AzureDevOps,
                        });
                    }
                }
            };
            // A repository with the same name as its project is addressable
            // without repeating it, so the project falls back to the repo name
            // -- which is what the REST API needs in the path either way.
            let project = project.or_else(|| Some(repo.clone()));
            return Ok(Remote {
                forge: Forge::AzureDevOps,
                host: host.to_string(),
                org,
                project,
                repo,
                clone_url,
            });
        }

        if host == "github.com" || host == "www.github.com" {
            let mut it = segments.iter();
            let owner = it.next().ok_or_else(|| Error::Incomplete {
                url: url.to_string(),
                missing: "owner",
                forge: Forge::GitHub,
            })?;
            let repo = it.next().ok_or_else(|| Error::Incomplete {
                url: url.to_string(),
                missing: "repository",
                forge: Forge::GitHub,
            })?;
            return Ok(Remote {
                forge: Forge::GitHub,
                host: "github.com".to_string(),
                org: (*owner).to_string(),
                project: None,
                repo: strip_git(repo),
                clone_url,
            });
        }

        Err(Error::UnknownHost(url.to_string()))
    }

    /// Human-readable identity, for the preview pane and messages.
    pub fn slug(&self) -> String {
        match &self.project {
            Some(p) => format!("{}/{}/{}", self.org, p, self.repo),
            None => format!("{}/{}", self.org, self.repo),
        }
    }

    /// The REST endpoint that opens a pull request.
    ///
    /// Azure DevOps only. GitHub's is not built here because `gh` is in the
    /// image and knows its own API; Azure DevOps has no equivalent CLI short of
    /// installing the Azure CLI plus its devops extension, which is a Python
    /// runtime and change to the image for a single POST.
    pub fn pull_request_url(&self) -> Option<String> {
        if self.forge != Forge::AzureDevOps {
            return None;
        }
        let project = self.project.as_deref()?;
        Some(format!(
            "https://{host}/{org}/{project}/_apis/git/repositories/{repo}/pullrequests?api-version=7.1",
            host = self.host,
            org = self.org,
            project = project,
            repo = self.repo,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_url_the_azure_devops_clone_button_gives_you() {
        // Note the organisation in the userinfo position: this is the default
        // the web UI offers, and the form most likely to be pasted in.
        let r = Remote::parse("https://inetse@dev.azure.com/inetse/MyProject/_git/MyRepo").unwrap();
        assert_eq!(r.forge, Forge::AzureDevOps);
        assert_eq!(r.host, "dev.azure.com");
        assert_eq!(r.org, "inetse");
        assert_eq!(r.project.as_deref(), Some("MyProject"));
        assert_eq!(r.repo, "MyRepo");
        assert_eq!(r.slug(), "inetse/MyProject/MyRepo");
    }

    /// The userinfo has to go. With a username in the URL git asks for that
    /// user's password before it sends anything, and fails with "could not read
    /// Username" -- while the gateway sits ready to inject a credential into a
    /// request git never makes.
    #[test]
    fn userinfo_is_stripped_from_the_clone_url() {
        for url in [
            "https://inetse@dev.azure.com/inetse/P/_git/R",
            "https://anything@dev.azure.com/inetse/P/_git/R",
        ] {
            let r = Remote::parse(url).unwrap();
            assert_eq!(r.clone_url, "https://dev.azure.com/inetse/P/_git/R");
            assert!(!r.clone_url.contains('@'), "{url}");
        }
        // A URL that never had userinfo must come back unchanged.
        let r = Remote::parse("https://dev.azure.com/inetse/P/_git/R").unwrap();
        assert_eq!(r.clone_url, "https://dev.azure.com/inetse/P/_git/R");
    }

    #[test]
    fn parses_azure_devops_without_a_project() {
        // A repo named the same as its project is addressable without it.
        let r = Remote::parse("https://dev.azure.com/inetse/_git/Shared").unwrap();
        assert_eq!(r.org, "inetse");
        assert_eq!(r.repo, "Shared");
        assert_eq!(
            r.project.as_deref(),
            Some("Shared"),
            "the REST path needs a project, and the repo name is the one it means"
        );
    }

    /// Long-lived remotes still point at the pre-2018 hostname, where the
    /// organisation is in the host rather than the path.
    #[test]
    fn parses_the_legacy_visualstudio_com_host() {
        let r = Remote::parse("https://inetse.visualstudio.com/MyProject/_git/MyRepo").unwrap();
        assert_eq!(r.forge, Forge::AzureDevOps);
        assert_eq!(r.org, "inetse");
        assert_eq!(r.project.as_deref(), Some("MyProject"));
        assert_eq!(r.repo, "MyRepo");
    }

    #[test]
    fn parses_github() {
        for url in [
            "https://github.com/octocat/Hello-World.git",
            "https://github.com/octocat/Hello-World",
            "https://github.com/octocat/Hello-World/",
        ] {
            let r = Remote::parse(url).unwrap();
            assert_eq!(r.forge, Forge::GitHub, "{url}");
            assert_eq!(r.org, "octocat");
            assert_eq!(r.repo, "Hello-World", "{url}");
            assert_eq!(r.project, None, "github has no project level");
            assert_eq!(r.slug(), "octocat/Hello-World");
        }
    }

    /// `_git` is the landmark, not a position. An organisation, project or repo
    /// can be named anything, including `_git`-adjacent things, and counting
    /// segments from the left breaks on the no-project form.
    #[test]
    fn azure_parsing_keys_off_the_git_landmark() {
        let r =
            Remote::parse("https://dev.azure.com/org/Project.With.Dots/_git/repo.name").unwrap();
        assert_eq!(r.project.as_deref(), Some("Project.With.Dots"));
        assert_eq!(r.repo, "repo.name");

        // `.git` is stripped from the repository, but a repo legitimately
        // containing "git" in its name keeps it.
        let r = Remote::parse("https://dev.azure.com/o/p/_git/gitignore.git").unwrap();
        assert_eq!(r.repo, "gitignore");
    }

    #[test]
    fn builds_the_pull_request_endpoint() {
        let r = Remote::parse("https://dev.azure.com/inetse/MyProject/_git/MyRepo").unwrap();
        assert_eq!(
            r.pull_request_url().unwrap(),
            "https://dev.azure.com/inetse/MyProject/_apis/git/repositories/MyRepo/pullrequests?api-version=7.1"
        );
        // GitHub goes through `gh`, so there is no URL to build.
        let gh = Remote::parse("https://github.com/o/r").unwrap();
        assert_eq!(gh.pull_request_url(), None);
    }

    /// An SSH remote cannot work: the policy allows HTTPS to a named host, and
    /// the sandbox has no key. Saying so beats a confusing network denial.
    #[test]
    fn ssh_remotes_are_rejected_with_an_explanation() {
        for url in [
            "git@github.com:octocat/Hello-World.git",
            "git@ssh.dev.azure.com:v3/inetse/MyProject/MyRepo",
            "ssh://git@ssh.dev.azure.com/v3/inetse/P/R",
        ] {
            let e = Remote::parse(url).unwrap_err();
            assert!(matches!(e, Error::Ssh(_)), "{url} -> {e:?}");
            assert!(e.to_string().contains("HTTPS"), "{url}");
        }
    }

    #[test]
    fn unknown_hosts_and_incomplete_paths_are_rejected() {
        assert!(matches!(
            Remote::parse("https://gitlab.com/o/r"),
            Err(Error::UnknownHost(_))
        ));
        assert!(matches!(
            Remote::parse("https://github.com/onlyowner"),
            Err(Error::Incomplete { .. })
        ));
        // Azure DevOps without the `_git` landmark is not a repository URL --
        // this is what a project's overview page looks like.
        assert!(matches!(
            Remote::parse("https://dev.azure.com/inetse/MyProject"),
            Err(Error::Incomplete { .. })
        ));
        let e = Remote::parse("https://dev.azure.com/inetse/MyProject").unwrap_err();
        assert!(e.to_string().contains("_git"), "{e}");
    }

    /// The exact URL the personal-org Clone button produced, kept because it is
    /// the form a user is most likely to paste and exercises userinfo, an org
    /// that repeats as the project, and a repo of the same name again.
    #[test]
    fn parses_the_personal_org_url() {
        let r = Remote::parse(
            "https://tobiaswadseth0266@dev.azure.com/tobiaswadseth0266/test/_git/test",
        )
        .unwrap();
        assert_eq!(r.org, "tobiaswadseth0266");
        assert_eq!(r.project.as_deref(), Some("test"));
        assert_eq!(r.repo, "test");
        assert_eq!(
            r.clone_url,
            "https://dev.azure.com/tobiaswadseth0266/test/_git/test"
        );
        assert_eq!(
            r.pull_request_url().unwrap(),
            "https://dev.azure.com/tobiaswadseth0266/test/_apis/git/repositories/test/pullrequests?api-version=7.1"
        );
    }

    /// Azure DevOps wants the token as a Basic *password*. Measured: sent as a
    /// bearer token the API answers 302 to an Entra sign-in page, so getting
    /// this wrong does not even look like an auth failure.
    #[test]
    fn azure_authenticates_as_basic_and_github_as_bearer() {
        let az = Forge::AzureDevOps.auth_header_expr();
        assert!(az.contains("Basic"), "{az}");
        assert!(az.contains("printf ':%s'"), "empty username: {az}");
        assert!(az.contains("base64 -w0"), "no line wrapping: {az}");
        assert!(az.contains("$AZURE_DEVOPS_PAT"), "{az}");
        assert!(!az.contains("Bearer"), "{az}");

        let gh = Forge::GitHub.auth_header_expr();
        assert!(gh.contains("Bearer $GITHUB_TOKEN"), "{gh}");
        assert!(!gh.contains("base64"), "{gh}");
    }

    /// A public repository must still clone with no provider attached, so the
    /// prelude has to degrade to a plain `git` rather than passing an empty
    /// `-c` argument -- which git rejects rather than ignoring.
    #[test]
    fn the_auth_prelude_degrades_without_a_credential() {
        for forge in [Forge::AzureDevOps, Forge::GitHub] {
            let p = git_auth_prelude(forge);
            assert!(
                p.contains(&format!("${{{}:-}}", forge.credential_env())),
                "{p}"
            );
            assert!(p.contains(r#"else git "$@""#), "plain git fallback: {p}");
            assert!(p.contains("http.extraHeader="), "{p}");
            // curl needs the bare header as well as git needing the -c form.
            assert!(p.contains("auth_header="), "{p}");
        }
    }

    /// Each forge names the policy block that grants it, so a denied publish can
    /// point at the thing to look at.
    #[test]
    fn each_forge_names_its_policy_rule_and_provider() {
        assert_eq!(Forge::AzureDevOps.policy_rule(), "azure_git");
        assert_eq!(Forge::AzureDevOps.provider_profile(), "azure-devops-pat");
        assert_eq!(Forge::GitHub.policy_rule(), "github_git");
        assert_eq!(Forge::GitHub.provider_profile(), "github");
        // Padded, so it can share a column with the other fields.
        assert_eq!(format!("{:<13}|", Forge::AzureDevOps), "azure-devops |");
    }
}
