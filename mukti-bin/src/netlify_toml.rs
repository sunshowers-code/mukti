// Copyright (c) The mukti Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::command::Alias;
use atomicwrites::{AtomicFile, OverwriteBehavior};
use color_eyre::{eyre::Context, Result};
use mukti_metadata::{ReleaseJson, ReleaseLocation};
use std::{fmt::Write as _, io::Write as _};

pub(crate) fn generate_netlify_redirects(
    release_json: &ReleaseJson,
    aliases: &[Alias],
    netlify_prefix: &str,
) -> Result<()> {
    let netlify_prefix = netlify_prefix.trim_end_matches('/');
    let mut out = String::with_capacity(4096);

    writeln!(&mut out, "# Generated by mukti\n")?;

    if let Some(range) = &release_json.latest {
        let latest_data = &release_json.ranges[range];
        let latest_locations = &latest_data.versions[&latest_data.latest];
        write_entries(
            &"latest",
            latest_locations,
            aliases,
            netlify_prefix,
            &mut out,
        );
    }

    for (range, data) in &release_json.ranges {
        if !data.is_prerelease {
            let locations = &data.versions[&data.latest];
            write_entries(range, locations, aliases, netlify_prefix, &mut out);
        }
        for (version, locations) in &data.versions {
            write_entries(version, locations, aliases, netlify_prefix, &mut out);
        }
    }

    let file = AtomicFile::new("_redirects", OverwriteBehavior::AllowOverwrite);
    file.write(|f| f.write_all(out.as_bytes()))
        .wrap_err("failed to write _redirects")?;

    Ok(())
}

fn write_entries(
    version: &dyn std::fmt::Display,
    locations: &[ReleaseLocation],
    aliases: &[Alias],
    netlify_prefix: &str,
    out: &mut String,
) {
    for location in locations {
        writeln!(
            out,
            "{}/{}/{} {} 302",
            netlify_prefix, version, location.target, location.url
        )
        .expect("writing to a string is infallible");
        if let Some(alias) = aliases.iter().find(|alias| alias.target == location.target) {
            writeln!(
                out,
                "{}/{}/{} {} 302",
                netlify_prefix, version, alias.alias, location.url
            )
            .expect("writing to a string is infallible");
        }
    }
}
