//! Policy templates, the mid-run widen, and the policy pane's body.
//!
//! The isolation is the reason this tool exists rather than claude-squad, so it
//! is deliberately visible: a named template at creation, the effective rules
//! in a pane, and a keybinding to widen or tighten egress while the agent runs.
//!
//! Templates are embedded in the binary and written to a temp file when the CLI
//! needs a path, the same trick [`crate::image`] uses for the build context, so
//! `sbx` works installed rather than only from a checkout.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use openshell_client::{Policy, PolicyRevision, PolicyUpdate};

use crate::pane;

/// A policy shipped with the binary, selectable by name.
pub struct Template {
    pub name: &'static str,
    /// One line, for `--help` and the pane.
    pub summary: &'static str,
    pub yaml: &'static str,
}

/// The templates, widest-denying first. Order is the order `sbx new --help`
/// lists them in, so it reads as a range rather than a set.
pub const TEMPLATES: [Template; 3] = [
    Template {
        name: "readonly-explore",
        summary: "clone and read; no egress for the agent at all",
        yaml: include_str!("../../../policies/readonly-explore.yaml"),
    },
    Template {
        name: "feature-work",
        summary: "clone, agent, push; nothing else reachable",
        yaml: include_str!("../../../policies/feature-work.yaml"),
    },
    Template {
        name: "net-open",
        summary: "feature-work plus the npm and PyPI registries",
        yaml: include_str!("../../../policies/net-open.yaml"),
    },
];

/// The template used when `--policy` is not given.
pub const DEFAULT_TEMPLATE: &str = "feature-work";

pub fn find(name: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == name)
}

/// Template names and summaries, for help text.
pub fn help() -> String {
    TEMPLATES
        .iter()
        .map(|t| format!("{:<17}{}", t.name, t.summary))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A policy resolved to a file the `openshell` CLI can be pointed at.
///
/// Owns the temp file when the policy came from a template, and removes it on
/// drop -- so it has to stay alive until `sandbox create` has run, not just
/// until the path has been read.
#[derive(Debug)]
pub struct Resolved {
    /// What to record in the session: the template name, or the path as given.
    pub label: String,
    path: PathBuf,
    temp: bool,
}

impl Resolved {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Resolved {
    fn drop(&mut self) {
        if self.temp {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no policy template or file `{spec}`\n\navailable templates:\n{available}")]
    Unknown { spec: String, available: String },
    #[error("could not write the {template} template to {path}: {source}")]
    Write {
        template: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Resolve `--policy`: a template name, or a path to a YAML file.
///
/// A name is tried first, so a file called `net-open.yaml` in the working
/// directory cannot silently shadow the template of that name -- but a spec
/// that looks like a path (`./net-open`, `policies/net-open.yaml`) is never
/// matched against a template, so the checked-in files stay usable directly.
pub fn resolve(spec: &str) -> Result<Resolved, Error> {
    let looks_like_path = spec.contains('/') || spec.ends_with(".yaml") || spec.ends_with(".yml");

    if !looks_like_path && let Some(t) = find(spec) {
        return materialize(t);
    }

    let path = PathBuf::from(spec);
    if path.is_file() {
        return Ok(Resolved {
            label: spec.to_string(),
            path,
            temp: false,
        });
    }
    Err(Error::Unknown {
        spec: spec.to_string(),
        available: help(),
    })
}

/// Write an embedded template out so the CLI can be given a path.
fn materialize(t: &Template) -> Result<Resolved, Error> {
    // The pid keeps concurrent invocations from sharing a file. Not a security
    // boundary: the content is a compile-time constant.
    let path =
        std::env::temp_dir().join(format!("sbx-policy-{}-{}.yaml", t.name, std::process::id()));
    fs::write(&path, t.yaml).map_err(|source| Error::Write {
        template: t.name.to_string(),
        path: path.clone(),
        source,
    })?;
    Ok(Resolved {
        label: t.name.to_string(),
        path,
        temp: true,
    })
}

/// The mid-run widen: the package registries.
///
/// Sent as a single `policy update`, because `--binary` applies to every
/// `--add-endpoint` in the invocation: splitting npm and PyPI the way
/// `net-open.yaml` splits them would mean two calls, two revisions and two
/// six-second waits. The cost is that node can reach PyPI and uv can reach npm.
///
/// What the gateway does with that is not what asking for it suggests. Measured
/// against 0.0.110, three `--add-endpoint` flags become *three* rules, one per
/// endpoint (`allow_pypi_org_443`, `allow_registry_npmjs_org_443`, ...), each
/// carrying the full binary list -- not one merged rule. Hence
/// [`preset_rule_names`] returning a list, and hence the pane rendering the
/// policy as it comes back rather than describing what was requested.
pub struct Preset {
    /// How to describe the preset to the user. Not the rule's name in the
    /// policy: `--rule-name` is rejected for a multi-endpoint update, so the
    /// gateway picks that, and reporting this string as the rule name would be
    /// a lie. Use [`preset_rule_names`] for what it is actually called.
    pub label: &'static str,
    /// `host:port`.
    pub endpoints: &'static [&'static str],
    /// A method class, not an allow-list. `rules:` alone is default-deny, but
    /// `access:` grants its whole method class; a registry fetch is thousands
    /// of unpredictable paths, so read-only says what is actually meant.
    pub access: &'static str,
    pub binaries: &'static [&'static str],
}

pub const REGISTRIES: Preset = Preset {
    label: "package registries (npm, PyPI)",
    endpoints: &[
        "registry.npmjs.org:443",
        "pypi.org:443",
        "files.pythonhosted.org:443",
    ],
    access: "read-only",
    // node covers npm and npx, which are JavaScript files with a `#!` line, so
    // the kernel-resolved exe is the interpreter. uv is a real binary. Plain
    // `pip` is deliberately not covered: its exe is a version-pinned
    // uv-managed interpreter path that would break on the next image rebuild.
    binaries: &["/usr/bin/node", "/usr/local/bin/uv"],
};

impl Preset {
    /// Add the preset's endpoints.
    pub fn widen(&self) -> PolicyUpdate {
        PolicyUpdate {
            add_endpoints: self
                .endpoints
                .iter()
                .map(|e| format!("{e}:{}:rest:enforce", self.access))
                .collect(),
            binaries: self.binaries.iter().map(|b| (*b).to_string()).collect(),
            // Only accepted with exactly one --add-endpoint, so it cannot be
            // set here; the gateway derives a rule name from the first host.
            rule_name: None,
            // The whole point is that the next request is governed by the new
            // rules, so returning before they load would be a lie.
            wait: true,
            ..Default::default()
        }
    }

    /// Remove them again. Removing every endpoint of a rule removes the rule,
    /// verified against 0.0.110.
    pub fn tighten(&self) -> PolicyUpdate {
        PolicyUpdate {
            remove_endpoints: self.endpoints.iter().map(|e| (*e).to_string()).collect(),
            wait: true,
            ..Default::default()
        }
    }
}

/// The rule keys the gateway actually created for the preset, if any.
///
/// `--rule-name` is rejected for a multi-endpoint update, so the names are the
/// gateway's to choose -- and it chooses one per endpoint, derived from the
/// host. Matching on the endpoints instead keeps "is this already applied?"
/// correct whatever it picks.
pub fn preset_rule_names(policy: &Policy, preset: &Preset) -> Vec<String> {
    policy
        .network_policies
        .iter()
        .filter(|(_, p)| {
            p.endpoints
                .iter()
                .any(|e| preset.endpoints.contains(&e.host_port().as_str()))
        })
        .map(|(key, _)| key.clone())
        .collect()
}

/// Render a revision as the policy pane's body.
///
/// Network first: it is the section that can be changed while the agent runs,
/// and the one worth reading. Filesystem and process come last, under a notice,
/// because they are frozen at creation -- and the gateway does not say so.
pub fn render(rev: &PolicyRevision, template: Option<&str>) -> String {
    let mut out = String::new();

    pane::section(&mut out, "policy");
    pane::field(&mut out, "template", template.unwrap_or("(none recorded)"));
    let revision = if rev.is_settled() {
        format!("{} (loaded)", rev.version)
    } else {
        format!("{} submitted, {} loaded", rev.version, rev.active_version)
    };
    pane::field(&mut out, "revision", revision);
    if !rev.policy_source.is_empty() {
        pane::field(&mut out, "source", &rev.policy_source);
    }
    if !rev.hash.is_empty() {
        // The first 12 characters are what the CLI itself prints on an update,
        // so the two can be compared by eye.
        pane::field(
            &mut out,
            "hash",
            rev.hash.chars().take(12).collect::<String>(),
        );
    }
    if !rev.is_settled() {
        pane::notice(
            &mut out,
            "a newer revision has been submitted; the rules below are the ones loaded",
        );
    }
    // The template is what the session was created from, and stays recorded
    // after a widen or a tighten has moved the policy away from it. Without
    // this the pane names a template whose rules are visibly not the ones
    // below, which reads as a bug rather than as history.
    if rev.version > 1 && template.is_some() {
        pane::notice(
            &mut out,
            "the network rules have been changed since creation; the template names",
        );
        pane::notice(
            &mut out,
            "what the session started from, not what it has now",
        );
    }
    if rev.policy_source == "global" {
        pane::notice(
            &mut out,
            "a gateway-global policy lock is in force and outranks this sandbox's own",
        );
    }

    let Some(policy) = &rev.policy else {
        out.push('\n');
        pane::notice(&mut out, "the gateway returned no policy payload");
        return out;
    };

    render_network(&mut out, policy);
    render_locked(&mut out, policy);
    out
}

fn render_network(out: &mut String, policy: &Policy) {
    out.push('\n');
    if policy.network_policies.is_empty() {
        pane::section(out, "network");
        pane::notice(out, "no network rules: nothing in this sandbox has egress");
        return;
    }

    for (key, rule) in &policy.network_policies {
        // The key and the display name differ (`github_git` vs `github-git`),
        // and the key is what `policy update` addresses, so it leads.
        let name = rule.name.as_deref().unwrap_or(key);
        let heading = if name == key {
            format!("network - {key}")
        } else {
            format!("network - {key}  ({name})")
        };
        pane::section(out, heading);

        if rule.binaries.is_empty() {
            pane::notice(out, "no binaries: this rule grants nothing");
        } else {
            // Every binary on its own row. A joined list is unreadable at the
            // pane's width, and the kernel-resolved path is the thing you have
            // to compare against a denial message character by character.
            for (i, b) in rule.binaries.iter().enumerate() {
                let label = if i == 0 { "binaries" } else { "" };
                pane::field(out, label, &b.path);
            }
        }

        for e in &rule.endpoints {
            let mut line = e.host_port();
            for v in [&e.protocol, &e.enforcement].into_iter().flatten() {
                let _ = write!(line, "  {v}");
            }
            match &e.access {
                Some(a) => {
                    let _ = write!(line, "  {a}");
                }
                // Absence is the meaningful case: no access class and a rules
                // block is default-deny, which is stricter than anything
                // `access:` can express. Saying nothing would read as an
                // oversight.
                None if !e.rules.is_empty() => line.push_str("  (rules only)"),
                None => line.push_str("  no access granted"),
            }
            if e.tls.as_deref() == Some("skip") {
                line.push_str("  tls:skip");
            }
            pane::field(out, "endpoint", line);

            for rule in &e.rules {
                if let Some(a) = &rule.allow {
                    pane::field(out, "", format!("allow  {} {}", a.method, a.path));
                }
                if let Some(d) = &rule.deny {
                    pane::field(out, "", format!("deny   {} {}", d.method, d.path));
                }
            }
            if e.access.is_some() && !e.rules.is_empty() {
                pane::notice(
                    out,
                    "access and rules together grant the union, not the intersection",
                );
            }
            if e.tls.as_deref() == Some("terminate") {
                pane::notice(
                    out,
                    "`tls: terminate` is deprecated; termination is automatic now",
                );
            }
        }
    }
}

/// The sections that cannot be changed after creation.
fn render_locked(out: &mut String, policy: &Policy) {
    let fs = &policy.filesystem_policy;
    out.push('\n');
    pane::section(out, "filesystem and process - locked at creation");

    pane::field(
        out,
        "workdir",
        if fs.include_workdir {
            "included"
        } else {
            "excluded"
        },
    );
    for (label, paths) in [("read-write", &fs.read_write), ("read-only", &fs.read_only)] {
        if paths.is_empty() {
            continue;
        }
        for (i, p) in paths.iter().enumerate() {
            pane::field(out, if i == 0 { label } else { "" }, p);
        }
    }
    if let Some(u) = &policy.process.run_as_user {
        let group = policy.process.run_as_group.as_deref().unwrap_or("-");
        pane::field(out, "run as", format!("{u}:{group}"));
    }

    // Measured, not assumed: `policy set` with an extra read_write path returns
    // "Policy version 4 loaded", `policy get --full` then reports the new path,
    // and every subsequent Landlock application still logs the creation-time
    // count. So the gateway's own answer for this section cannot be trusted on
    // a live sandbox, and the pane has to say which half it is showing.
    pane::notice(
        out,
        "these are as submitted, not necessarily as enforced: Landlock is applied",
    );
    pane::notice(
        out,
        "at creation, and a later change is accepted and reported but never takes",
    );
    pane::notice(out, "effect. Recreate the session to change them.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use openshell_client::{Binary, Endpoint, MethodPath, NetworkPolicy, Rule};

    fn revision(policy: Option<Policy>) -> PolicyRevision {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "active_version": 1,
            "hash": "90715775a1ec73bed8bcf1b289d245562fe2dfec88ba9dee0c26fc6ebe02eab6",
            "policy_source": "sandbox",
            "status": "effective",
        }))
        .map(|mut r: PolicyRevision| {
            r.policy = policy;
            r
        })
        .unwrap()
    }

    #[test]
    fn every_template_is_findable_and_parses_as_yaml() {
        for t in &TEMPLATES {
            assert!(find(t.name).is_some(), "{} not findable", t.name);
            assert!(!t.summary.is_empty());
            // Not a YAML parser -- the crate has none -- but the shape a policy
            // must have is cheap to assert, and a template that lost its
            // network section would deny everything without saying so.
            assert!(t.yaml.contains("version: 1"), "{}", t.name);
            assert!(t.yaml.contains("filesystem_policy:"), "{}", t.name);
            assert!(t.yaml.contains("run_as_user: sandbox"), "{}", t.name);
            assert!(t.yaml.contains("/dev/pts"), "{} must allow a pty", t.name);
        }
        assert!(find(DEFAULT_TEMPLATE).is_some());
        assert!(find("no-such-template").is_none());
    }

    /// Strip `#` comments, so a test about what a template *does* is not
    /// confounded by a template explaining what it no longer does.
    fn directives(yaml: &str) -> String {
        yaml.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The deprecation the gateway warns about on every create. It is worth a
    /// test rather than a comment because the warning is only visible in the
    /// sandbox log, which nothing reads by default.
    #[test]
    fn no_template_carries_the_deprecated_tls_key() {
        for t in &TEMPLATES {
            assert!(
                !directives(t.yaml).contains("tls: terminate"),
                "{} still sets tls: terminate",
                t.name
            );
        }
    }

    /// readonly-explore has to actually deny what it claims to: no model API,
    /// and no push. A template that quietly allowed either would make the
    /// strict end of the range a lie.
    #[test]
    fn readonly_explore_denies_the_model_api_and_push() {
        let y = find("readonly-explore").unwrap().yaml;
        assert!(!y.contains("api.anthropic.com"));
        assert!(!y.contains("git-receive-pack"), "push must not be allowed");
        assert!(y.contains("git-upload-pack"), "fetch must still work");
    }

    #[test]
    fn net_open_is_feature_work_plus_registries() {
        let feature = find("feature-work").unwrap().yaml;
        let open = find("net-open").unwrap().yaml;
        for host in [
            "api.anthropic.com",
            "github.com",
            "api.github.com",
            "git-receive-pack",
        ] {
            assert!(feature.contains(host), "feature-work lost {host}");
            assert!(open.contains(host), "net-open lost {host}");
        }
        for host in ["registry.npmjs.org", "pypi.org", "files.pythonhosted.org"] {
            assert!(
                !feature.contains(host),
                "feature-work must not reach {host}"
            );
            assert!(open.contains(host), "net-open must reach {host}");
        }
        // npm is a `#!` script, so the exe the gateway matches is the
        // interpreter. Listing only /usr/bin/npm denies every install.
        assert!(open.contains("/usr/bin/node"));
    }

    #[test]
    fn resolves_a_template_by_name_to_a_real_file() {
        let r = resolve("feature-work").unwrap();
        assert_eq!(r.label, "feature-work");
        assert!(r.path().is_file());
        let written = fs::read_to_string(r.path()).unwrap();
        assert_eq!(written, find("feature-work").unwrap().yaml);

        // The temp file is the resolver's, and must not outlive it.
        let path = r.path().to_path_buf();
        drop(r);
        assert!(!path.exists(), "the temp policy must be cleaned up");
    }

    /// A path is relative to the working directory, which under `cargo test` is
    /// the package root rather than the workspace root -- hence the manifest
    /// dir rather than a bare `policies/...`.
    #[test]
    fn resolves_a_path_and_leaves_it_alone() {
        let spec = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../policies/feature-work.yaml"
        );
        let r = resolve(spec).unwrap();
        assert_eq!(r.label, spec);
        let path = r.path().to_path_buf();
        drop(r);
        assert!(path.exists(), "a file the user owns must never be removed");
    }

    /// A spec that looks like a path is never matched against a template, so
    /// the checked-in `policies/*.yaml` stay directly usable -- and a template
    /// name still wins over a same-named file in the working directory, so
    /// `--policy net-open` cannot be hijacked by a local file.
    #[test]
    fn a_template_name_and_a_path_do_not_shadow_each_other() {
        assert!(resolve("net-open").unwrap().label == "net-open");
        assert!(matches!(resolve("./net-open"), Err(Error::Unknown { .. }),));
        assert!(matches!(resolve("nope"), Err(Error::Unknown { .. })));
        // The error has to be actionable, so it lists what is available.
        let e = resolve("nope").unwrap_err().to_string();
        for t in &TEMPLATES {
            assert!(e.contains(t.name), "{e}");
        }
    }

    #[test]
    fn the_widen_preset_round_trips() {
        let widen = REGISTRIES.widen();
        assert_eq!(widen.add_endpoints.len(), REGISTRIES.endpoints.len());
        assert!(
            widen
                .add_endpoints
                .contains(&"pypi.org:443:read-only:rest:enforce".to_string())
        );
        assert!(widen.binaries.contains(&"/usr/bin/node".to_string()));
        assert!(widen.wait, "a widen that has not loaded yet is not a widen");
        assert!(!REGISTRIES.label.is_empty());
        assert!(
            widen.rule_name.is_none(),
            "the CLI rejects --rule-name with several --add-endpoint, so the \
             gateway names the rules and there is nothing to pass"
        );
        assert!(!widen.is_empty());

        let tighten = REGISTRIES.tighten();
        assert_eq!(tighten.remove_endpoints.len(), REGISTRIES.endpoints.len());
        assert!(
            tighten
                .remove_endpoints
                .contains(&"pypi.org:443".to_string())
        );
        assert!(tighten.add_endpoints.is_empty());
        assert!(!tighten.is_empty());
        // Removal addresses host:port only; carrying the access class over from
        // the widen spec would not match anything.
        for e in &tighten.remove_endpoints {
            assert_eq!(e.matches(':').count(), 1, "{e} is not host:port");
        }
    }

    fn policy_with(key: &str, host: &str) -> Policy {
        let mut p = Policy::default();
        p.network_policies.insert(
            key.to_string(),
            NetworkPolicy {
                name: Some(key.to_string()),
                endpoints: vec![Endpoint {
                    host: host.to_string(),
                    port: 443,
                    ..Default::default()
                }],
                binaries: vec![Binary {
                    path: "/usr/bin/node".into(),
                }],
            },
        );
        p
    }

    /// The gateway names the rule, not us: `--rule-name` is rejected for a
    /// multi-endpoint update. So "is the preset applied?" has to be answered
    /// from the endpoints -- matching on a name we chose would answer no to a
    /// rule that is right there, and the widen key would toggle nothing.
    #[test]
    fn the_preset_is_recognised_under_a_gateway_chosen_name() {
        for key in ["sbx-registries", "registry-npmjs-org", "whatever_it_picks"] {
            let p = policy_with(key, "registry.npmjs.org");
            assert_eq!(
                preset_rule_names(&p, &REGISTRIES),
                vec![key.to_string()],
                "not found under {key}"
            );
        }

        let unrelated = policy_with("github_git", "github.com");
        assert!(preset_rule_names(&unrelated, &REGISTRIES).is_empty());
    }

    #[test]
    fn renders_a_rule_with_its_binaries_and_endpoints() {
        let body = render(
            &revision(Some(policy_with("npm", "registry.npmjs.org"))),
            Some("net-open"),
        );
        assert!(body.contains("net-open"), "{body}");
        assert!(body.contains("1 (loaded)"), "{body}");
        assert!(body.contains("90715775a1ec"), "{body}");
        assert!(body.contains("registry.npmjs.org:443"), "{body}");
        assert!(body.contains("/usr/bin/node"), "{body}");
        // Truncated, not the whole 64-character hash.
        assert!(!body.contains("90715775a1ec73"), "{body}");
    }

    /// An endpoint with neither an access class nor rules grants nothing, and
    /// one with rules and no access is default-deny. Rendering both as a bare
    /// host would make a policy that denies look like one that allows.
    #[test]
    fn an_endpoint_says_what_it_actually_grants() {
        let mut p = Policy::default();
        p.network_policies.insert(
            "r".into(),
            NetworkPolicy {
                name: None,
                binaries: vec![Binary {
                    path: "/usr/bin/git".into(),
                }],
                endpoints: vec![
                    Endpoint {
                        host: "a.example".into(),
                        port: 443,
                        rules: vec![Rule {
                            allow: Some(MethodPath {
                                method: "GET".into(),
                                path: "/x".into(),
                            }),
                            deny: None,
                        }],
                        ..Default::default()
                    },
                    Endpoint {
                        host: "b.example".into(),
                        port: 443,
                        ..Default::default()
                    },
                    Endpoint {
                        host: "c.example".into(),
                        port: 443,
                        access: Some("read-only".into()),
                        rules: vec![Rule {
                            allow: Some(MethodPath {
                                method: "GET".into(),
                                path: "/y".into(),
                            }),
                            deny: None,
                        }],
                        ..Default::default()
                    },
                ],
            },
        );
        let body = render(&revision(Some(p)), None);

        assert!(body.contains("a.example:443  (rules only)"), "{body}");
        assert!(body.contains("allow  GET /x"), "{body}");
        assert!(body.contains("b.example:443  no access granted"), "{body}");
        // The union caveat is the thing that bit us in testing: an allow-list
        // next to `access: read-only` reads as a restriction but is not one.
        assert!(body.contains("union, not the intersection"), "{body}");
    }

    /// The finding that cost the most to establish: a live filesystem change is
    /// accepted, reported as effective, and never enforced. If the pane renders
    /// the section without saying so, it actively misleads.
    #[test]
    fn the_locked_sections_are_labelled_as_unenforceable() {
        let body = render(&revision(Some(Policy::default())), None);
        assert!(body.contains("locked at creation"), "{body}");
        assert!(body.contains("never takes"), "{body}");
        assert!(body.contains("Recreate the session"), "{body}");
    }

    #[test]
    fn a_sandbox_with_no_network_rules_says_so() {
        let body = render(&revision(Some(Policy::default())), None);
        assert!(
            body.contains("nothing in this sandbox has egress"),
            "{body}"
        );
    }

    /// Between submitting a widen and the supervisor loading it, the pane must
    /// not claim the new rules are in force.
    #[test]
    fn an_unsettled_revision_is_flagged() {
        let mut rev = revision(Some(Policy::default()));
        rev.version = 4;
        rev.active_version = 3;
        let body = render(&rev, None);
        assert!(body.contains("4 submitted, 3 loaded"), "{body}");
        assert!(body.contains("the ones loaded"), "{body}");
    }

    /// A widen or a tighten moves the policy away from its template. Naming the
    /// template without saying so reads as the pane showing the wrong rules.
    #[test]
    fn a_changed_policy_says_the_template_is_only_history() {
        let mut rev = revision(Some(Policy::default()));
        rev.version = 2;
        rev.active_version = 2;
        let body = render(&rev, Some("net-open"));
        assert!(body.contains("changed since creation"), "{body}");

        // At revision 1 nothing has changed, so the caveat would be noise.
        let fresh = render(&revision(Some(Policy::default())), Some("net-open"));
        assert!(!fresh.contains("changed since creation"), "{fresh}");

        // And with no template recorded there is nothing to disclaim.
        let mut anon = revision(Some(Policy::default()));
        anon.version = 2;
        anon.active_version = 2;
        assert!(!render(&anon, None).contains("changed since creation"));
    }

    #[test]
    fn a_global_lock_is_called_out() {
        let mut rev = revision(Some(Policy::default()));
        rev.policy_source = "global".into();
        assert!(render(&rev, None).contains("global policy lock"));
    }

    /// `policy get` without `--full` returns metadata only. Rendering that as
    /// an empty policy would show a sandbox as having no rules at all.
    #[test]
    fn a_missing_payload_is_not_an_empty_policy() {
        let body = render(&revision(None), Some("feature-work"));
        assert!(body.contains("no policy payload"), "{body}");
        assert!(!body.contains("locked at creation"), "{body}");
    }
}
