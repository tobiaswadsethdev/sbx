//! Toolchains: what a sandbox can build, and what that lets it reach.
//!
//! A sandbox that can only clone and read is a sandbox that can only write code
//! nobody has compiled. The base image carries node and python because the
//! community image does; anything else -- the .NET SDK, a Rust toolchain -- has
//! to be put there, and *where* it is put is the whole design question.
//!
//! It is put in the image, at build time, for the same reason the agent's own
//! version is: `/usr/local` is not writable by the sandbox user, and no policy
//! template lets a sandbox reach a download host. An agent asked to install its
//! own toolchain would fail twice over, and widening the policy far enough for it
//! to succeed would hand every session a route to arbitrary tarballs on the
//! internet. So the toolchain is the image's business.
//!
//! **A toolchain is three things, not one.** The install is the obvious part. The
//! other two are why this is a module rather than a longer Dockerfile:
//!
//! * an **image variant**, tagged by the toolchains in it -- `sbx-base:dotnet`,
//!   `sbx-base:dotnet-rust` -- layered onto the base image, so docker shares the
//!   base's several gigabytes and a Rust session does not carry the .NET SDK;
//! * the **registry endpoints** it cannot work without, each bound to the binary
//!   that reaches it, imposed on the session that asked for the toolchain and on
//!   no other. `net-open.yaml` already argues the other half of this: an endpoint
//!   granted to a sandbox with no binary that can use it is decoration, and it
//!   named crates.io as exactly that. This is where crates.io arrives, alongside
//!   a cargo that can reach it.
//!
//! The binary paths below are **kernel-resolved** paths, not what is on `PATH`.
//! The gateway matches `/proc/<pid>/exe`, so a symlink in `/usr/local/bin` is
//! invisible to it and an interpreted wrapper resolves to its interpreter --
//! which is why npm's rule names `/usr/bin/node`. Getting one wrong produces a
//! denial naming a path the policy appears to contain, and the tests here check
//! each one against the Dockerfile layer that installs it.

use std::path::Path;

use openshell_client::PolicyUpdate;

use crate::session::IMAGE_REPO;

/// A package registry a toolchain cannot work without.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registry {
    pub host: &'static str,
    pub port: u16,
    /// Kernel-resolved paths of the binaries granted this endpoint. Never empty:
    /// an endpoint rule with no binaries grants nothing.
    pub binaries: &'static [&'static str],
}

/// A toolchain installable into a sandbox image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toolchain {
    /// The name on the command line, and the slug in the image tag.
    pub name: &'static str,
    /// One line, for `--help` and the create form.
    pub summary: &'static str,
    /// The Dockerfile fragment layered onto the base image.
    pub layer: &'static str,
    /// What this toolchain has to reach to fetch dependencies.
    pub registries: &'static [Registry],
    /// Files whose presence in a checkout means this toolchain is wanted.
    ///
    /// A bare name is matched exactly and a `*.ext` entry by extension. See
    /// [`detect`], which is what the create form uses to arrive already ticked.
    pub markers: &'static [&'static str],
}

/// The access class a registry endpoint is granted.
///
/// `read-only` rather than an allow-list of paths, and rather than `full`, for
/// the reason `net-open.yaml` spells out: a registry fetch is thousands of
/// unpredictable paths, so an allow-list would either be wrong or be `/**`, and
/// "GET anything here, write nothing" is what is actually meant. Publishing a
/// package is not something a sandboxed agent should be able to do by accident.
const ACCESS: &str = "read-only";

/// The toolchains, in the order that decides an image tag.
///
/// Alphabetical, and the order is load-bearing: [`tag`] joins the slugs in this
/// order, so `--toolchain rust --toolchain dotnet` and `--toolchain
/// dotnet,rust` name one image rather than building two identical ones under
/// different tags.
pub const TOOLCHAINS: [Toolchain; 3] = [
    Toolchain {
        name: "dotnet",
        summary: "the .NET SDK, and nuget",
        layer: include_str!("../../../images/sbx-base/toolchains/dotnet.dockerfile"),
        registries: &[Registry {
            host: "api.nuget.org",
            port: 443,
            // The muxer, at its real path. `/usr/local/bin/dotnet` is a symlink
            // to this and the kernel reports the target, so naming the symlink
            // would deny every restore.
            binaries: &["/usr/local/dotnet/dotnet"],
        }],
        // A solution or any project file. `global.json` catches the repository
        // that pins an SDK version without a project at its root.
        markers: &[
            "*.sln",
            "*.slnx",
            "*.csproj",
            "*.fsproj",
            "*.vbproj",
            "global.json",
        ],
    },
    Toolchain {
        name: "node",
        summary: "node and npm (already in the base image), and the npm registry",
        layer: include_str!("../../../images/sbx-base/toolchains/node.dockerfile"),
        registries: &[Registry {
            host: "registry.npmjs.org",
            port: 443,
            // node, not npm: `/usr/bin/npm` is a JavaScript file behind a
            // `#!/usr/bin/env node` line, so the kernel-resolved exe is the
            // interpreter. Listing `/usr/bin/npm` denies every install with a
            // message naming a path you can see is in the policy.
            binaries: &["/usr/bin/node"],
        }],
        markers: &["package.json"],
    },
    Toolchain {
        name: "rust",
        summary: "rustc, cargo, fmt and clippy, and crates.io",
        layer: include_str!("../../../images/sbx-base/toolchains/rust.dockerfile"),
        registries: &[
            // The sparse index and the crate downloads are two hosts, and cargo
            // needs both: the index alone resolves versions and then fails to
            // fetch anything.
            Registry {
                host: "index.crates.io",
                port: 443,
                binaries: &["/usr/local/rust/bin/cargo"],
            },
            Registry {
                host: "static.crates.io",
                port: 443,
                binaries: &["/usr/local/rust/bin/cargo"],
            },
        ],
        markers: &["Cargo.toml"],
    },
];

/// Where a built image records what it carries, one `name version` per line.
///
/// Written by the layers themselves, so the image is the source of truth about
/// itself -- the same reasoning as the session metadata living inside the
/// sandbox. A tag can be typed by hand; this cannot.
pub const MANIFEST_PATH: &str = "/usr/local/share/sbx/toolchains";

pub fn find(name: &str) -> Option<&'static Toolchain> {
    TOOLCHAINS.iter().find(|t| t.name == name)
}

/// Names and summaries, for help text.
pub fn help() -> String {
    TOOLCHAINS
        .iter()
        .map(|t| format!("{:<9}{}", t.name, t.summary))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Comma-separated names, for one `--help` line and for error messages.
pub fn names() -> String {
    TOOLCHAINS
        .iter()
        .map(|t| t.name)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("no toolchain `{name}`; available: {available}")]
    Unknown { name: String, available: String },
}

/// Resolve names to toolchains: de-duplicated, and in [`TOOLCHAINS`] order.
///
/// Order is imposed rather than preserved, because the result decides an image
/// tag and two spellings of one request must not build two images. Blanks are
/// dropped so `--toolchain ""` and a trailing comma mean "none" rather than
/// being a name that does not exist.
pub fn resolve(names: &[String]) -> Result<Vec<&'static Toolchain>, Error> {
    let mut wanted: Vec<&'static Toolchain> = Vec::new();
    for name in names {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let found = find(name).ok_or_else(|| Error::Unknown {
            name: name.to_string(),
            available: self::names(),
        })?;
        if !wanted.contains(&found) {
            wanted.push(found);
        }
    }
    Ok(TOOLCHAINS
        .iter()
        .filter(|t| wanted.contains(t))
        .collect::<Vec<_>>())
}

/// The names of `chains`, for a session record and for display.
pub fn labels(chains: &[&'static Toolchain]) -> Vec<String> {
    chains.iter().map(|t| t.name.to_string()).collect()
}

/// The image tag carrying exactly these toolchains.
///
/// The base image for none, so a session that wants no toolchain is unchanged by
/// this whole module and needs no second image built.
pub fn tag(chains: &[&'static Toolchain]) -> String {
    if chains.is_empty() {
        return crate::session::IMAGE.to_string();
    }
    format!(
        "{IMAGE_REPO}:{}",
        chains.iter().map(|t| t.name).collect::<Vec<_>>().join("-")
    )
}

/// The Dockerfile for a variant image: the base, plus one layer per toolchain.
///
/// `FROM sbx-base:latest` rather than a longer base Dockerfile with conditional
/// steps, so the several gigabytes of base image are built once and shared by
/// every variant -- and so a variant's build is only ever the toolchains asked
/// for. It also keeps one thing true that a conditional build would quietly
/// break: the base image is what a session with no toolchain runs, byte for byte.
///
/// `USER root` and back again, because the base image ends as the sandbox user
/// and every layer here installs into `/usr/local`. Ending anywhere else would
/// hand the agent a root shell.
pub fn dockerfile(chains: &[&'static Toolchain]) -> String {
    let mut out = String::from("# syntax=docker/dockerfile:1\n");
    out.push_str(
        "#\n# Generated by `sbx image build --toolchain`; see crates/sbx/src/toolchain.rs.\n",
    );
    out.push_str(&format!("FROM {}\n\nUSER root\n\n", crate::session::IMAGE));
    // The manifest's directory, made once here rather than by each layer: they
    // all append to the same file and none of them owns it.
    out.push_str(&format!(
        "RUN mkdir -p {dir}\n\n",
        dir = MANIFEST_PATH
            .rsplit_once('/')
            .map(|(d, _)| d)
            .unwrap_or("/")
    ));
    for chain in chains {
        out.push_str(chain.layer.trim_end());
        out.push_str("\n\n");
    }
    out.push_str("USER sandbox\n");
    out
}

/// How far into a checkout [`detect`] looks.
///
/// The root and one level under it. `src/Thing/Thing.csproj` is the ordinary
/// shape of a .NET repository and would be missed by the root alone; two levels
/// would find `node_modules/*/package.json` and every vendored crate, and would
/// cost a walk of the whole tree to do it.
const DETECT_DEPTH: usize = 1;

/// Directories never descended into.
///
/// Build output and vendored dependencies, all of which contain exactly the
/// markers being looked for -- `target/package/*/Cargo.toml` is a real path --
/// and none of which says anything about what the repository is written in.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "bin",
    "obj",
    "dist",
    "build",
    "vendor",
];

/// The toolchains a checkout looks like it needs.
///
/// Evidence, not configuration: a repository with a `Cargo.toml` in it needs a
/// Rust toolchain, and making that a question the create form asks from scratch
/// every time is asking something the repository has already answered. The form
/// ticks these and they can be unticked, which is the same shape as the provider
/// guesswork beside it.
///
/// Read from the *local* checkout, which is only ever how a remote was named --
/// the sandbox clones `origin`. The two can disagree, in the same way the form's
/// drift note already says they can: a `Cargo.toml` committed but not pushed
/// would tick rust for a clone that does not have one yet. Harmless in that
/// direction -- an unused toolchain is a bigger image and an endpoint nothing
/// reaches -- and the wrong direction is one keystroke to fix.
pub fn detect(root: &Path) -> Vec<&'static Toolchain> {
    let mut found: Vec<&'static Toolchain> = Vec::new();
    let look = |dir: &Path, found: &mut Vec<&'static Toolchain>| -> Vec<std::path::PathBuf> {
        let mut subdirs = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return subdirs;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) {
                    subdirs.push(entry.path());
                }
                continue;
            }
            for chain in TOOLCHAINS.iter() {
                if chain.markers.iter().any(|m| matches(m, &name)) && !found.contains(&chain) {
                    found.push(chain);
                }
            }
        }
        subdirs
    };

    let mut level = vec![root.to_path_buf()];
    for _ in 0..=DETECT_DEPTH {
        let mut next = Vec::new();
        for dir in &level {
            next.extend(look(dir, &mut found));
        }
        level = next;
    }

    TOOLCHAINS.iter().filter(|t| found.contains(t)).collect()
}

/// Whether a file name satisfies a marker: an exact name, or `*.ext`.
fn matches(marker: &str, name: &str) -> bool {
    match marker.strip_prefix("*") {
        Some(ext) => name.len() > ext.len() && name.ends_with(ext),
        None => marker == name,
    }
}

/// The policy updates opening these toolchains' registries.
///
/// One update per distinct binary list, for the constraint that shapes
/// [`crate::endpoints::Lists::updates`] too: `--binary` applies to *every*
/// `--add-endpoint` in an invocation, so merging cargo's endpoints with dotnet's
/// would let cargo reach nuget and dotnet reach crates.io. Two calls at six
/// seconds each is the price of the rules meaning what they say.
///
/// `rule_name` is left unset: the gateway rejects it for a multi-endpoint update
/// and its own derived name is the clearer of the two anyway.
pub fn updates(chains: &[&'static Toolchain]) -> Vec<PolicyUpdate> {
    let mut groups: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    for registry in chains.iter().flat_map(|t| t.registries) {
        let binaries: Vec<String> = registry.binaries.iter().map(|b| (*b).to_string()).collect();
        let endpoint = format!("{}:{}:{ACCESS}:rest:enforce", registry.host, registry.port);
        match groups.iter_mut().find(|(b, _)| *b == binaries) {
            Some((_, endpoints)) if endpoints.contains(&endpoint) => {}
            Some((_, endpoints)) => endpoints.push(endpoint),
            None => groups.push((binaries, vec![endpoint])),
        }
    }

    groups
        .into_iter()
        .map(|(binaries, add_endpoints)| PolicyUpdate {
            add_endpoints,
            binaries,
            rule_name: None,
            // The agent is started by the seeder moments later and a build is
            // often the first thing it does, so returning before the rules load
            // would be a denial in the feed with a working policy behind it.
            wait: true,
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_resolved_deduplicated_and_ordered() {
        let spec = |names: &[&str]| {
            resolve(&names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .map(|got| got.iter().map(|t| t.name).collect::<Vec<_>>())
        };

        assert_eq!(spec(&["dotnet"]).unwrap(), ["dotnet"]);
        // Order is imposed, not preserved: both spellings must name one image.
        assert_eq!(spec(&["rust", "dotnet"]).unwrap(), ["dotnet", "rust"]);
        assert_eq!(spec(&["dotnet", "rust"]).unwrap(), ["dotnet", "rust"]);
        // Asked for twice is asked for once.
        assert_eq!(spec(&["rust", "rust"]).unwrap(), ["rust"]);
        // A blank is "none", not a name that does not exist -- `--toolchain ""`
        // and a trailing comma both arrive here as one.
        assert_eq!(spec(&["", " "]).unwrap(), [] as [&str; 0]);
        assert_eq!(spec(&[]).unwrap(), [] as [&str; 0]);

        // And a typo names itself, with the alternatives.
        let err = spec(&["dotnett"]).unwrap_err();
        assert_eq!(
            err,
            Error::Unknown {
                name: "dotnett".into(),
                available: names(),
            }
        );
        assert!(err.to_string().contains("dotnet"), "{err}");
    }

    /// The tag is what decides whether an image is rebuilt, so it has to be a
    /// pure function of the *set* of toolchains.
    #[test]
    fn the_tag_names_the_set_and_nothing_else() {
        let of = |names: &[&str]| {
            let owned: Vec<String> = names.iter().map(|s| s.to_string()).collect();
            tag(&resolve(&owned).unwrap())
        };
        assert_eq!(of(&[]), crate::session::IMAGE, "no toolchain, no variant");
        assert_eq!(of(&["dotnet"]), "sbx-base:dotnet");
        assert_eq!(of(&["dotnet", "rust"]), "sbx-base:dotnet-rust");
        assert_eq!(
            of(&["rust", "dotnet"]),
            of(&["dotnet", "rust"]),
            "one set, one image"
        );
        // Every tag has to be a legal docker reference, or the build fails with
        // a message about parsing rather than about toolchains.
        for chain in TOOLCHAINS {
            let name = chain.name;
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '.'),
                "`{name}` would not be a usable tag component"
            );
        }
    }

    /// A variant is the base image plus layers, and it must drop back to the
    /// sandbox user -- the base ends as `sandbox`, every layer needs root, and
    /// ending as root would hand the agent a root shell.
    #[test]
    fn the_generated_dockerfile_layers_onto_the_base_and_drops_privileges() {
        let chains = resolve(&["dotnet".to_string(), "rust".to_string()]).unwrap();
        let df = dockerfile(&chains);

        assert!(
            df.contains(&format!("FROM {}", crate::session::IMAGE)),
            "{df}"
        );
        assert!(
            df.contains("USER root"),
            "the layers install into /usr/local"
        );
        assert!(
            df.trim_end().ends_with("USER sandbox"),
            "a variant must end as the sandbox user:\n{df}"
        );
        // The manifest's directory is not in the base image, and no layer owns
        // it, so the wrapper has to make it before any layer appends.
        let dir = df.find("mkdir -p /usr/local/share/sbx").expect("mkdir");
        let first_layer = df.find("ARG DOTNET_CHANNEL").expect("the dotnet layer");
        assert!(dir < first_layer, "the manifest dir comes first");

        // Both layers, and only the ones asked for.
        assert!(df.contains("ARG RUST_CHANNEL"), "the rust layer is missing");
        assert!(
            !df.contains("/usr/bin/npm"),
            "node was not asked for:\n{df}"
        );
    }

    /// Every layer has to record itself, or `sbx doctor` reads an image that
    /// looks empty and says a variant carries nothing.
    #[test]
    fn every_layer_writes_the_manifest_the_doctor_reads() {
        for chain in TOOLCHAINS {
            assert!(
                chain.layer.contains(MANIFEST_PATH),
                "the {} layer must append to {MANIFEST_PATH}",
                chain.name
            );
            assert!(
                chain
                    .layer
                    .contains(&format!("printf '{} %s\\n'", chain.name)),
                "the {} layer must record its own name and version",
                chain.name
            );
        }
    }

    /// The one thing about this that is easy to get wrong and impossible to see:
    /// the gateway matches the kernel-resolved binary, so every path in a
    /// registry rule has to be the path the layer actually installs to. A
    /// mismatch denies every fetch while naming a path the policy appears to
    /// hold.
    #[test]
    fn every_registry_binary_is_a_path_its_layer_installs() {
        for chain in TOOLCHAINS {
            for registry in chain.registries {
                assert!(
                    !registry.binaries.is_empty(),
                    "{} grants {} to nothing",
                    chain.name,
                    registry.host
                );
                for binary in registry.binaries {
                    assert!(
                        binary.starts_with('/'),
                        "`{binary}` is not an absolute path"
                    );
                    assert!(
                        chain.layer.contains(binary),
                        "the {} layer never mentions `{binary}`, so either the \
                         install moved or the rule is for a path that is not there",
                        chain.name
                    );
                    // Symlinks are invisible to the gateway: it reports the
                    // target. A rule naming the convenience symlink would deny
                    // everything.
                    assert!(
                        !binary.starts_with("/usr/local/bin/"),
                        "`{binary}` is the symlink, not the resolved binary"
                    );
                }
            }
        }
    }

    /// The rules have to mean what they say: cargo may reach crates.io and
    /// dotnet may reach nuget, and neither may reach the other's.
    #[test]
    fn endpoints_are_granted_only_to_their_own_toolchains_binaries() {
        let chains = resolve(&["dotnet".to_string(), "rust".to_string()]).unwrap();
        let updates = updates(&chains);
        assert_eq!(updates.len(), 2, "one call per binary list: {updates:#?}");

        for update in &updates {
            assert!(update.wait, "a build may be the first thing the agent does");
            assert!(update.remove_endpoints.is_empty(), "nothing is taken away");
            assert!(
                update.rule_name.is_none(),
                "the gateway rejects a name on a multi-endpoint update"
            );
        }

        let for_binary = |binary: &str| -> Vec<String> {
            updates
                .iter()
                .filter(|u| u.binaries.iter().any(|b| b == binary))
                .flat_map(|u| u.add_endpoints.clone())
                .collect()
        };

        let cargo = for_binary("/usr/local/rust/bin/cargo");
        assert_eq!(
            cargo.len(),
            2,
            "the sparse index and the downloads: {cargo:?}"
        );
        assert!(cargo.iter().all(|e| e.contains("crates.io")), "{cargo:?}");
        assert!(
            !cargo.iter().any(|e| e.contains("nuget")),
            "cargo must not reach nuget: {cargo:?}"
        );

        let dotnet = for_binary("/usr/local/dotnet/dotnet");
        assert_eq!(dotnet, ["api.nuget.org:443:read-only:rest:enforce"]);

        // Read-only, and enforced: a sandboxed agent publishing a package is
        // not a thing this should make possible by accident.
        for update in &updates {
            for endpoint in &update.add_endpoints {
                assert!(endpoint.ends_with(":read-only:rest:enforce"), "{endpoint}");
            }
        }
    }

    /// The other half of "one set, one image": asking for nothing must cost
    /// nothing, not an empty variant build.
    #[test]
    fn no_toolchains_means_no_variant_and_no_updates() {
        assert!(updates(&[]).is_empty());
        assert_eq!(tag(&[]), crate::session::IMAGE);
    }

    /// The form arrives ticked from evidence, so the evidence has to be the
    /// shapes real repositories actually have -- including the .NET one, whose
    /// project files are a level down from the root.
    #[test]
    fn a_checkout_is_read_for_what_it_is_written_in() {
        let root = std::env::temp_dir().join(format!("sbx-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let names = |path: &std::path::Path| -> Vec<&'static str> {
            detect(path).iter().map(|t| t.name).collect()
        };
        let touch = |rel: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        };

        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(
            names(&root),
            [] as [&str; 0],
            "an empty checkout asks for nothing"
        );

        touch("Cargo.toml");
        assert_eq!(names(&root), ["rust"]);

        // One level down, which is where a .NET project usually lives.
        touch("src/Thing/Thing.csproj");
        assert_eq!(names(&root), ["rust"], "two levels down is not looked at");
        touch("src/Thing.csproj");
        assert_eq!(names(&root), ["dotnet", "rust"], "and in TOOLCHAINS order");

        // Build output and vendored dependencies hold every marker there is, and
        // say nothing about the repository.
        let _ = std::fs::remove_file(root.join("Cargo.toml"));
        let _ = std::fs::remove_file(root.join("src/Thing.csproj"));
        touch("node_modules/left-pad/package.json");
        touch("target/package/thing-1.0.0/Cargo.toml");
        assert_eq!(names(&root), [] as [&str; 0], "output is not evidence");

        touch("package.json");
        assert_eq!(names(&root), ["node"]);

        // A missing directory is not an error; the picker can point at a path
        // that has since gone.
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(names(&root), [] as [&str; 0]);
    }

    /// `*.ext` must not match the extension on its own: a file literally called
    /// `.sln` is not a solution, and matching it would tick dotnet for a
    /// repository holding an editor config.
    #[test]
    fn a_marker_extension_needs_something_in_front_of_it() {
        assert!(matches("*.sln", "Thing.sln"));
        assert!(!matches("*.sln", ".sln"));
        assert!(!matches("*.sln", "sln"));
        assert!(matches("Cargo.toml", "Cargo.toml"));
        assert!(
            !matches("Cargo.toml", "cargo.toml"),
            "case matters on Linux"
        );
        assert!(!matches("Cargo.toml", "Cargo.toml.orig"));
    }

    /// A registry granted to a session that has no binary able to use it is the
    /// "unreachable decoration" `net-open.yaml` refuses to ship. The inverse is
    /// worse: a toolchain with no registry cannot fetch a dependency, which is
    /// most of the reason to have it.
    #[test]
    fn every_toolchain_can_reach_a_registry() {
        for chain in TOOLCHAINS {
            assert!(
                !chain.registries.is_empty(),
                "{} has no registry, so it could not restore anything",
                chain.name
            );
            assert!(!chain.summary.is_empty(), "{} has no summary", chain.name);
            assert!(
                !chain.markers.is_empty(),
                "{} would never be detected in a checkout",
                chain.name
            );
        }
        assert!(help().contains("dotnet"), "{}", help());
    }
}
