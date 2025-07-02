#![warn(clippy::all)]
#![warn(missing_docs, missing_debug_implementations, rust_2018_idioms)]
//! Release tool to update all versions of everything
//! inside the crate at the same time to the same version
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Error};
use clap::{Parser, Subcommand};
use toml_edit::{Document, Formatted, InlineTable, Item, KeyMut, Value};

#[derive(Debug, Subcommand)]
enum SubCommand {
    Update { newver: String },
    Check { newver: String },
}

impl SubCommand {
    fn version(&self) -> &String {
        match self {
            SubCommand::Update { newver } | SubCommand::Check { newver } => newver,
        }
    }
}

#[derive(Debug, Parser)]
struct Args {
    /// how cargo invoked this; cargo chews up the first argument
    /// so this should be completely ignored
    #[clap(hide(true))]
    _cargo_invoked_as: String,

    #[command(subcommand)]
    cmd: SubCommand,

    /// Don't print anything
    #[arg(short, long)]
    quiet: bool,
}

impl Args {
    /// Get the version without any leading 'v'
    fn version(&self) -> &str {
        let ver = self.cmd.version();
        if let Some(ver) = ver.strip_prefix('v') {
            ver
        } else {
            ver
        }
    }
    fn write(&self) -> bool {
        matches!(self.cmd, SubCommand::Update { newver: _ })
    }
    fn check(&self) -> bool {
        matches!(self.cmd, SubCommand::Check { newver: _ })
    }
    fn maybe_write(
        &self,
        path: &(impl AsRef<Path> + ?Sized),
        document: &Document,
    ) -> anyhow::Result<()> {
        let path = path.as_ref();
        if self.write() {
            if !self.quiet {
                println!("{} was updated", path.display());
            }
            fs::write(path, document.to_string())?;
        } else if !self.quiet {
            println!("{} needs to be updated", path.display());
        }

        Ok(())
    }
}

fn main() -> Result<(), Error> {
    let cli = Args::parse();

    // first read the top level Cargo.cli
    let base = std::fs::read_to_string("Cargo.toml")?;
    let mut doc = base.parse::<Document>()?;
    // get the [workspace] section
    let workspace = doc
        .get_mut("workspace")
        .ok_or(anyhow!("No [workspace] section in top level"))?;

    let mut root_updated = false;
    let mut found_workspace_version = false;
    if let Some(packages) = workspace.get_mut("package") {
        // The workspace root declares a `packages` section. The packages we find
        // within the workspace are likely to have `package.version.workspace = true`
        // which means they inherit the version from the workspace root.

        match packages.get_mut("version") {
            None => {
                // do nothing. this just means the workspace root does not declare a version
            }
            Some(Item::Value(v)) => {
                found_workspace_version = true;
                root_updated = check_version(v, "Cargo.toml", &cli);
            }
            Some(_) => bail!("version in [workspace.package] wasn't a string"),
        }
    }

    // find the members array inside the workspace
    let members = workspace
        .get("members")
        .ok_or(anyhow!("No members in [workspace] section"))?
        .as_array()
        .ok_or(anyhow!("members must be an array"))?;

    // save these members into a hashmap for easy lookup later. We will
    // only change [dependencies] that point to one of these, and we need
    // to check each one to see if it's one we care about
    let members_lookup = members
        .iter()
        .map(|v| v.as_str().expect("member wasn't a string").to_string())
        .collect::<HashSet<String>>();

    let mut some_difference_found = root_updated;

    // work on each subdirectory (each member of the workspace)
    for member in members {
        // calculate the path of the inner member
        let inner_path: PathBuf = [member.as_str().unwrap(), "Cargo.toml"].iter().collect();
        // and load into a parsed yaml document
        let inner = std::fs::read_to_string(&inner_path)
            .context(format!("Can't read {}", inner_path.display()))?;
        let mut inner = inner.parse::<Document>()?;

        // now find the [package] section
        let package = inner.get_mut("package").ok_or(anyhow!(format!(
            "no [package] section in {}",
            inner_path.display()
        )))?;
        // which contains: version = "xxx"; mutable since we might change it
        let version = package.get_mut("version");

        // keep track of if we changed anything, to avoid unnecessary rewrites
        let mut changed = false;

        // extract the value; we want a better error here in case we can't find
        // it or if the version couldn't be parsed as a string
        match version {
            None => {
                // TODO: We could just set the version...
                bail!(format!("No version in {}", inner_path.display()))
            }
            Some(Item::Value(v)) => {
                changed |= check_version(v, inner_path.display().to_string(), &cli);
            }
            Some(Item::Table(tbl)) if is_workspace_true(tbl) && found_workspace_version => {
                // do nothing; we already found the version in the workspace root
            }
            Some(_) => bail!(format!(
                "version in {} wasn't a string",
                inner_path.display()
            )),
        }

        // now work on the [dependencies] section. We only care about
        // dependencies with names that are one of the subdirectories
        // we found when we parsed the members section at the top level
        // so we filter using the hashset created earlier
        // dependencies consist of a table of "name = { inline_table }"
        // entries. We skip those that don't have that format (the short
        // form of "name = version" for example)
        if let Some(deps) = inner.get_mut("dependencies") {
            if let Some(deps) = deps.as_table_mut() {
                // build an iterator of K,V pairs for each dependency
                // and do the filtering here for items in the members_lookup
                for dep in deps
                    .iter_mut()
                    .filter(|dep| members_lookup.contains(dep.0.get()))
                {
                    // call fixup_version for this dependency, which
                    // might make a change if the version was wrong
                    if let Some(inline_table) = dep.1.as_inline_table_mut() {
                        changed |= update_dep_ver(&dep.0, inline_table, &cli);
                    }
                }
            };
        }
        if changed {
            cli.maybe_write(&inner_path, &inner)?;
        }
        some_difference_found |= changed;
    }

    // Write the root Cargo.toml if we made any changes but only after we no longer
    // reference `workspace` or `members` which hold a reference to the document
    if root_updated {
        cli.maybe_write("Cargo.toml", &doc)?;
    }

    if cli.check() && some_difference_found {
        bail!("There were differences")
    }
    if cli.check() && !cli.quiet {
        println!("All files had the correct version");
    }
    Ok(())
}

/// Verify and/or update the version of a dependency
///
/// Given a dependency and the table of attributes, check the
/// "version" attribute and make sure it matches what we expect
/// from the command line arguments
///
/// * `key` - the name of this dependency
/// * `dep` - the table of K/V pairs describing the dependency
/// * `opts` - the command line arguments passed in
///
/// Returns true if any changes were made
fn update_dep_ver(key: &KeyMut<'_>, dep: &mut InlineTable, opts: &Args) -> bool {
    let v = dep.get_mut("version").unwrap();
    check_version(v, format!("dependency for {}", key.get()), opts)
}

/// Check and/or set the version
///
/// Check the version value provided and optionally
/// log and/or update it, based on the command line args
///
/// Arguments:
///
/// * `v` - the version to verify/change
/// * `source` - the text of where this version came from
/// * `opts` - the command line arguments
///
/// Returns `true` if a change was made, `false` otherwise
fn check_version<S: AsRef<str>>(v: &mut Value, source: S, opts: &Args) -> bool {
    if let Some(old) = v.as_str() {
        if old != opts.version() {
            if !opts.quiet {
                println!(
                    "Version for {} was {old} want {}{}",
                    source.as_ref(),
                    opts.version(),
                    if opts.write() { " (fixing)" } else { "" },
                );
            }
            *v = Value::String(Formatted::new(opts.version().to_string()));
            return true;
        }
    }
    false
}

/// Returns true if the provided table has a `workspace = true` entry
///
/// # Arguments
/// * `tbl` - a table retrieved from a TOML document. For example,
///
///    ```toml
///    [package]
///    version.workspace = true
///    ```
///
///   `tbl` would be the value of `package.version` in this case.
fn is_workspace_true(tbl: &toml_edit::Table) -> bool {
    if let Some(Item::Value(Value::Boolean(v))) = tbl.get("workspace") {
        *v.value()
    } else {
        false
    }
}
