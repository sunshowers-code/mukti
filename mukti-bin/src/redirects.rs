// Copyright (c) The mukti Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::command::Alias;
use atomicwrites::{AtomicFile, OverwriteBehavior};
use camino::Utf8Path;
use clap::ValueEnum;
use color_eyre::eyre::{bail, Context, Result};
use core::fmt;
use mukti_metadata::{MuktiReleasesJson, ReleaseVersionData, VersionRange};
use semver::Version;
use std::{collections::HashMap, fmt::Write as _, io::Write as _};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum RedirectFlavor {
    /// Netlify _redirects: purely static
    Netlify,

    /// Cloudflare _redirects: uses :version splats along with some static redirects
    Cloudflare,
}

pub(crate) fn generate_redirects(
    release_json: &MuktiReleasesJson,
    aliases: &[Alias],
    flavor: RedirectFlavor,
    prefix: &str,
    out_dir: &Utf8Path,
) -> Result<()> {
    if release_json.projects.len() != 1 {
        bail!(
            "mukti-bin currently only supports one project, {} found",
            release_json.projects.len()
        );
    }

    let project = release_json
        .projects
        .values()
        .next()
        .expect("release_json has one project");

    let netlify_prefix = prefix.trim_end_matches('/');
    let mut out = String::with_capacity(4096);

    writeln!(
        &mut out,
        "# Generated by mukti with redirect flavor {:?}\n",
        flavor
    )?;

    let mut redirects = Vec::new();

    if let Some(range) = &project.latest {
        let latest_range_data = &project.ranges[range];
        let latest_version_data = &latest_range_data.versions[&latest_range_data.latest];
        append_redirect_list(
            RedirectVersion::Latest,
            latest_version_data,
            aliases,
            netlify_prefix,
            &mut redirects,
        );
    }

    for (range, data) in &project.ranges {
        if !data.is_prerelease {
            let version_data = &data.versions[&data.latest];
            append_redirect_list(
                RedirectVersion::Range(*range),
                version_data,
                aliases,
                netlify_prefix,
                &mut redirects,
            );
        }
        for (version, version_data) in &data.versions {
            append_redirect_list(
                RedirectVersion::Version(version.clone()),
                version_data,
                aliases,
                netlify_prefix,
                &mut redirects,
            );
        }
    }

    match flavor {
        RedirectFlavor::Netlify => {
            // Just write out the redirect list.
            for redirect in &redirects {
                writeln!(out, "{}", redirect).expect("writing to a string is infallible");
            }
        }
        RedirectFlavor::Cloudflare => {
            // Attempt to derive wildcards from the list of redirects.
            let wildcards = WildcardStore::build(&redirects);

            // First write unmatched/static redirects.
            for redirect in &wildcards.unmatched {
                writeln!(out, "{}", redirect).expect("writing to a string is infallible");
            }

            // Then write wildcards, since they should match less tightly than static redirects.
            for wildcard in &wildcards.wildcards {
                writeln!(out, "{}", wildcard).expect("writing to a string is infallible");
            }
        }
    }

    let file = AtomicFile::new(
        out_dir.join("_redirects"),
        OverwriteBehavior::AllowOverwrite,
    );
    file.write(|f| f.write_all(out.as_bytes()))
        .wrap_err("failed to write _redirects")?;

    Ok(())
}

// In a WildcardStore, wildcards and unmatched together cover the full set of redirects
#[derive(Debug)]
struct WildcardStore<'a> {
    wildcards: Vec<Wildcard<'a>>,
    unmatched: Vec<Redirect>,
}

impl<'a> WildcardStore<'a> {
    fn build(redirects: &'a [Redirect]) -> Self {
        // from_components -> ((kind, to_components) -> list of redirects)
        let mut url_matches: HashMap<_, HashMap<_, Vec<_>>> = HashMap::new();
        let mut unmatched = Vec::new();

        for redirect in redirects {
            // Only consider full versions.
            if !matches!(redirect.version, RedirectVersion::Version(_)) {
                unmatched.push(redirect.clone());
                continue;
            }

            let version_str = redirect.version.to_string();
            let (from_start, from_end) = match redirect.from.split_once(&version_str) {
                Some((start, end)) => (start, end),
                None => {
                    unmatched.push(redirect.clone());
                    continue;
                }
            };

            let to_components: Vec<_> = redirect.to.split(&version_str).collect();

            url_matches
                .entry((from_start, from_end))
                .or_default()
                .entry((redirect.kind, to_components))
                .or_default()
                .push(redirect);
        }

        // For each from key, look through all the to keys and find the most common one.
        let mut wildcards = Vec::new();

        for ((from_start, from_end), mut to_maps) in url_matches {
            // (kind, to_components, redirects)
            let mut best_to: Option<(RedirectKind, &[_], &[_])> = None;

            for ((kind, to_components), redirects) in &to_maps {
                if let Some((_, _, best_redirects)) = &best_to {
                    if redirects.len() > best_redirects.len() {
                        best_to = Some((*kind, to_components, redirects));
                    }
                } else {
                    best_to = Some((*kind, to_components, redirects));
                }
            }

            if let Some((kind, to_components, best_redirects)) = best_to {
                let wildcard = Wildcard {
                    kind,
                    from_components: (from_start, from_end),
                    to_components: to_components.to_vec(),
                    matching_redirects: best_redirects.to_vec(),
                };
                wildcards.push(wildcard);

                // Everything here is covered by the wildcard. (to_vec is required to avoid
                // borrowing issues.)
                let ktc = (kind, to_components.to_vec());
                to_maps.remove(&ktc);
            }

            // Anything left goes into unmatched.
            for (_, redirects) in to_maps {
                unmatched.extend(redirects.into_iter().cloned());
            }
        }

        // Sort the wildcard and unmatched lists.
        wildcards.sort_unstable_by_key(|wildcard| (wildcard.kind, wildcard.from_components));
        unmatched.sort();

        for wildcard in &wildcards {
            eprintln!(
                "found wildcard (matches {} redirects): {wildcard}",
                wildcard.matching_redirects.len()
            );
        }

        Self {
            wildcards,
            unmatched,
        }
    }
}

#[derive(Debug)]
struct Wildcard<'a> {
    // The version can only show up once in the redirect "from", therefore two components
    kind: RedirectKind,
    from_components: (&'a str, &'a str),
    to_components: Vec<&'a str>,
    matching_redirects: Vec<&'a Redirect>,
}

impl Wildcard<'_> {
    const VERSION_PLACEHOLDER: &'static str = ":version";
}

impl fmt::Display for Wildcard<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (from_start, from_end) = self.from_components;
        let to = self.to_components.join(Self::VERSION_PLACEHOLDER);

        write!(
            f,
            "{from_start}{}{from_end} {to} 302",
            Self::VERSION_PLACEHOLDER,
        )
    }
}

fn append_redirect_list(
    version: RedirectVersion,
    version_data: &ReleaseVersionData,
    aliases: &[Alias],
    prefix: &str,
    out: &mut Vec<Redirect>,
) {
    out.push(Redirect {
        version: version.clone(),
        kind: RedirectKind::Release,
        from: format!("{}/{}/release", prefix, version),
        to: version_data.release_url.clone(),
        code: 302,
    });

    for location in &version_data.locations {
        out.push(Redirect {
            version: version.clone(),
            kind: RedirectKind::Location,
            from: format!(
                "{}/{}/{}.{}",
                prefix, version, location.target, location.format
            ),
            to: location.url.clone(),
            code: 302,
        });
        for alias in aliases.iter().filter(|alias| {
            alias.target_format.target == location.target
                && alias.target_format.format == location.format
        }) {
            out.push(Redirect {
                version: version.clone(),
                kind: RedirectKind::Alias,
                from: format!("{}/{}/{}", prefix, version, alias.alias),
                to: location.url.clone(),
                code: 302,
            });
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
struct Redirect {
    version: RedirectVersion,
    kind: RedirectKind,
    from: String,
    to: String,
    code: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
enum RedirectKind {
    // Order here determines sort order for `Redirect`.
    Release,
    Location,
    Alias,
}

impl fmt::Display for Redirect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.from, self.to, self.code)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
enum RedirectVersion {
    Latest,
    Range(VersionRange),
    Version(Version),
}

impl fmt::Display for RedirectVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Latest => write!(f, "latest"),
            Self::Range(range) => write!(f, "{}", range),
            Self::Version(version) => write!(f, "{}", version),
        }
    }
}
