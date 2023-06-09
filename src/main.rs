use anyhow::Result;
use cargo_metadata::{MetadataCommand, Package};
use clap::Parser;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashMap;
use std::env::current_dir;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, process};
use which::which;

#[derive(Deserialize)]
struct CargoToml {
    #[serde(default)]
    workspace: Workspace,
}

#[derive(Default, Deserialize)]
struct Workspace {
    metadata: Metadata,
}

#[derive(Default, Deserialize)]
struct Metadata {
    groups: HashMap<String, Vec<String>>,
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Run cargo commands on a group of crates in a workspace"
)]
struct Args {
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Parser, Debug)]
enum Command {
    Run { group: String },
    Test { group: String },
    Build { group: String },
    Check { group: String },
    List { group: Option<String> },
}

impl CargoToml {
    fn find(cwd: &Path) -> Result<PathBuf> {
        cwd.ancestors()
            .find_map(|p| p.join("Cargo.toml").exists().then(|| p.join("Cargo.toml")))
            .ok_or(anyhow::anyhow!("Cargo.toml not found"))
    }

    fn load(manifest_path: &Path) -> Result<Self> {
        let cargo_toml_contents = fs::read_to_string(&manifest_path)?;
        Ok(toml::from_str::<CargoToml>(&cargo_toml_contents)?)
    }
}

fn get_group_crates<'a>(
    group_patterns: &[String],
    metadata: &'a cargo_metadata::Metadata,
) -> Result<impl Iterator<Item = &'a Package>> {
    let mut crates_by_package = Vec::new();
    let mut crates_by_path = Vec::new();
    for pattern in group_patterns {
        if let Some(path_glob) = pattern.strip_prefix("pkg:") {
            crates_by_package.push(Glob::new(path_glob)?)
        } else if let Some(crate_glob) = pattern.strip_prefix("path:") {
            crates_by_path.push(Glob::new(crate_glob)?)
        } else {
            // By default we assume it's a crate glob, like cargo
            crates_by_path.push(Glob::new(pattern)?)
        }
    }

    let crates_by_package = Arc::new(make_glob_set(crates_by_package)?);
    let crates_by_path = Arc::new(make_glob_set(crates_by_path)?);

    Ok(metadata
        .workspace_packages()
        .into_iter()
        .filter(move |package| {
            crates_by_package.is_match(&package.name)
                || crates_by_path
                    .is_match(get_package_path_relative_to_workspace(package, &metadata))
        }))
}

fn execute_on_group(cwd: &Path, subcommand: &str, group: &str) -> Result<()> {
    let manifest_path = CargoToml::find(cwd)?;
    let cargo_toml = CargoToml::load(&manifest_path)?;

    let Some(crates) = cargo_toml.workspace.metadata.groups.get(group) else {
        return Err(anyhow::anyhow!("Group {} not found", group));
    };

    let metadata = MetadataCommand::new().manifest_path(manifest_path).exec()?;

    let cargo = which("cargo")?;
    let mut cmd = process::Command::new(cargo);
    cmd.current_dir(cwd).arg(subcommand);

    for member in get_group_crates(crates, &metadata)? {
        cmd.arg("-p").arg(&member.name);
    }

    println!("Running command: {:?}", cmd);

    let result = cmd.spawn()?.wait()?;

    process::exit(result.code().unwrap_or(1));
}

fn get_package_path_relative_to_workspace(
    package: &Package,
    metadata: &cargo_metadata::Metadata,
) -> PathBuf {
    package
        .manifest_path
        .strip_prefix(metadata.workspace_root.as_str())
        .expect("package path should be child of workspace root")
        .parent()
        .unwrap()
        .into()
}

fn make_glob_set(globs: Vec<Glob>) -> Result<GlobSet> {
    let mut glob_set_builder = GlobSetBuilder::new();
    for glob in globs {
        glob_set_builder.add(glob);
    }

    Ok(glob_set_builder.build()?)
}

fn print_groups(cwd: &Path) -> Result<()> {
    let cargo_toml_path = CargoToml::find(cwd)?;
    let cargo_toml = CargoToml::load(&cargo_toml_path)?;
    let metadata = MetadataCommand::new()
        .manifest_path(&cargo_toml_path)
        .exec()?;

    for (group, crates) in cargo_toml.workspace.metadata.groups {
        println!("[{}]", group);
        for package in get_group_crates(&crates, &metadata)? {
            println!("  {}", package.name);
        }
    }

    Ok(())
}

fn print_group(cwd: &Path, group: &str) -> Result<()> {
    let cargo_toml_path = CargoToml::find(cwd)?;
    let metadata = MetadataCommand::new()
        .manifest_path(&cargo_toml_path)
        .exec()?;
    let cargo_toml = CargoToml::load(&cargo_toml_path)?;
    let crates = cargo_toml
        .workspace
        .metadata
        .groups
        .get(group)
        .ok_or(anyhow::anyhow!("Group {} not found", group))?;

    println!("[{}]", group);
    for package in get_group_crates(crates, &metadata)? {
        println!("  {}", package.name);
    }

    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    let cwd = args.cwd.unwrap_or_else(|| current_dir().unwrap());

    match args.command {
        Command::Run { group } => execute_on_group(&cwd, "run", &group)?,
        Command::Test { group } => execute_on_group(&cwd, "test", &group)?,
        Command::Build { group } => execute_on_group(&cwd, "build", &group)?,
        Command::Check { group } => execute_on_group(&cwd, "check", &group)?,
        Command::List { group: None } => print_groups(&cwd)?,
        Command::List { group: Some(group) } => print_group(&cwd, &group)?,
    };

    Ok(())
}
