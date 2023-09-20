use anyhow::Result;
use cargo_metadata::{MetadataCommand, Package};
use clap::{Args as ClapArgs, Parser};
use colored::*;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashMap;
use std::env::current_dir;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, process};
use tracing::info;
use which::which;

#[derive(Deserialize)]
struct RootCargoToml {
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

trait Options {
    fn add_to_command(&self, _cmd: &mut process::Command) {}
}

// Common flags like --release
#[derive(Parser, Debug)]
struct CommandOptions<Specific = DefaultSpecificOptions>
where
    Specific: Parser + ClapArgs,
{
    #[arg(long)]
    release: bool,
    #[command(flatten)]
    specific: Specific,
}

impl<T> Options for CommandOptions<T>
where
    T: Options + Parser + ClapArgs,
{
    fn add_to_command(&self, cmd: &mut process::Command) {
        let Self { release, specific } = self;
        if *release {
            cmd.arg("--release");
        }
        specific.add_to_command(cmd);
    }
}

#[derive(Parser, Debug)]
struct DefaultSpecificOptions;

impl Options for DefaultSpecificOptions {}

// Clippy-specific flags like --fix
#[derive(Parser, Debug)]
struct ClippyOptions {
    #[arg(long)]
    fix: bool,
    #[arg(long)]
    allow_dirty: bool,
}

impl Options for ClippyOptions {
    fn add_to_command(&self, cmd: &mut process::Command) {
        let Self { fix, allow_dirty } = self;
        if *fix {
            cmd.arg("--fix");
        }
        if *allow_dirty {
            cmd.arg("--allow-dirty");
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Run cargo commands on a group of crates in a workspace",
    override_usage = "Usage: cargo groups [OPTIONS] <COMMAND>"
)]
#[clap(bin_name = "cargo")]
struct Args {
    _subcommand_name: String,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[command(flatten)]
    manifest: clap_cargo::Manifest,
    #[command(subcommand)]
    command: Command,
}

#[derive(Parser, Debug)]
enum Command {
    /// Test a group of crates
    #[command(override_usage = "Usage: cargo groups test [OPTIONS] <GROUP>")]
    Test {
        group: String,
        #[command(flatten)]
        features: clap_cargo::Features,
        #[command(flatten)]
        options: CommandOptions,
    },
    /// Build a group of crates
    #[command(override_usage = "Usage: cargo groups build [OPTIONS] <GROUP>")]
    Build {
        group: String,
        #[command(flatten)]
        features: clap_cargo::Features,
        #[command(flatten)]
        options: CommandOptions,
    },
    /// Check a group of crates
    #[command(override_usage = "Usage: cargo groups check [OPTIONS] <GROUP>")]
    Check {
        group: String,
        #[command(flatten)]
        features: clap_cargo::Features,
        #[command(flatten)]
        options: CommandOptions,
    },
    /// Run clippy on a group of crates
    #[command(override_usage = "Usage: cargo groups clippy [OPTIONS] <GROUP>")]
    Clippy {
        group: String,
        #[command(flatten)]
        features: clap_cargo::Features,
        #[command(flatten)]
        options: CommandOptions<ClippyOptions>,
    },
    /// List the groups in the workspace. Add a group name to list the crates in that specific group
    #[command(override_usage = "Usage: cargo groups list [GROUP]")]
    List { group: Option<String> },
}

impl RootCargoToml {
    fn find(cwd: &Path, manifest_path: &Option<PathBuf>) -> Result<PathBuf> {
        if let Some(manifest_path) = manifest_path {
            return Ok(manifest_path.clone());
        }

        cwd.ancestors()
            .find_map(|p| p.join("Cargo.toml").exists().then(|| p.join("Cargo.toml")))
            .ok_or(anyhow::anyhow!("Cargo.toml not found"))
    }

    fn load(manifest_path: &Path) -> Result<Self> {
        let cargo_toml_contents = fs::read_to_string(&manifest_path)?;
        Ok(toml::from_str::<RootCargoToml>(&cargo_toml_contents)?)
    }
}

fn add_features(cmd: &mut process::Command, features: &clap_cargo::Features) {
    if features.no_default_features {
        cmd.arg("--no-default-features");
    }

    if features.all_features {
        cmd.arg("--all-features");
    }

    if !features.features.is_empty() {
        cmd.arg("--features");
    }

    for feature in &features.features {
        cmd.arg(feature);
    }
}

fn make_glob_set(globs: Vec<Glob>) -> Result<GlobSet> {
    let mut glob_set_builder = GlobSetBuilder::new();
    for glob in globs {
        glob_set_builder.add(glob);
    }

    Ok(glob_set_builder.build()?)
}

struct WorkspaceInfo {
    cwd: PathBuf,
    metadata: cargo_metadata::Metadata,
    cargo_toml: RootCargoToml,
}

impl WorkspaceInfo {
    fn from_args(args: &Args) -> Result<Self> {
        let cwd = args.cwd.clone().unwrap_or_else(|| current_dir().unwrap());
        let cargo_toml_path = RootCargoToml::find(&cwd, &args.manifest.manifest_path)?;
        let metadata = MetadataCommand::new()
            .manifest_path(&cargo_toml_path)
            .exec()?;
        let cargo_toml = RootCargoToml::load(&cargo_toml_path)?;

        Ok(Self {
            cwd,
            metadata,
            cargo_toml,
        })
    }

    fn print_groups(&self) -> Result<()> {
        if self.cargo_toml.workspace.metadata.groups.is_empty() {
            println!("No groups found");
            return Ok(());
        }

        for (group, crates) in &self.cargo_toml.workspace.metadata.groups {
            println!("[{}]", group);
            for package in self.get_group_crates(&crates, false)? {
                self.print_package(package);
            }
        }

        Ok(())
    }

    fn print_package(&self, package: &Package) {
        println!(
            "  {} {}",
            package.name,
            self.get_package_path_relative_to_workspace(&package)
                .display()
                .to_string()
                .dimmed()
        );
    }

    fn print_group(&self, group: &str) -> Result<()> {
        let crates = self
            .cargo_toml
            .workspace
            .metadata
            .groups
            .get(group)
            .ok_or(anyhow::anyhow!("Group {} not found", group))?;

        println!("[{}]", group);
        for package in self.get_group_crates(crates, false)? {
            self.print_package(package);
        }

        Ok(())
    }

    fn get_group_crates(
        &self,
        group_patterns: &[String],
        only_run_top_level: bool,
    ) -> Result<Vec<&Package>> {
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

        let packages_iter = self
            .metadata
            .workspace_packages()
            .into_iter()
            .filter(move |package| {
                crates_by_package.is_match(&package.name)
                    || crates_by_path.is_match(self.get_package_path_relative_to_workspace(package))
            });

        if only_run_top_level {
            // Then build a map of the packages that we want to build
            let mut packages: HashMap<_, _> = packages_iter
                .clone()
                .map(|package| (package.name.clone(), package))
                .collect();

            // Then iterate through packages and remove dependent packages,
            // i.e. if package A depends on package B, we don't need to actively
            // build package B. This is important because if another package C depends
            // on a different version of B, we'll get a build error.
            for package in packages_iter {
                for dependency in package.dependencies.clone() {
                    if packages.contains_key(&dependency.name) {
                        packages.remove(&dependency.name);
                    }
                }
            }

            Ok(packages.into_iter().map(|(_, package)| package).collect())
        } else {
            Ok(packages_iter.collect())
        }
    }

    fn get_package_path_relative_to_workspace(&self, package: &Package) -> PathBuf {
        package
            .manifest_path
            .strip_prefix(self.metadata.workspace_root.as_str())
            .expect("package path should be child of workspace root")
            .parent()
            .unwrap()
            .into()
    }

    fn execute_on_group<T>(
        &self,
        subcommand: &str,
        group: &str,
        features: clap_cargo::Features,
        options: T,
        // Only run the top level packages, i.e. don't run dependencies
        // useful for commands like `cargo check` where the dependencies
        // are checked as part of the top level package, but not so useful
        // for commands like `cargo test` where the dependencies' tests are
        // not run.
        only_run_top_level: bool,
    ) -> Result<()>
    where
        T: Options,
    {
        let Some(crates) = self.cargo_toml.workspace.metadata.groups.get(group) else {
            return Err(anyhow::anyhow!("Group {} not found", group));
        };

        let cargo = which("cargo")?;
        let mut cmd = process::Command::new(cargo);
        cmd.current_dir(&self.cwd).arg(subcommand);
        add_features(&mut cmd, &features);
        options.add_to_command(&mut cmd);

        for member in self.get_group_crates(crates, only_run_top_level)? {
            cmd.arg("-p").arg(&member.name);
        }

        info!("Running command: {:?}", cmd);

        let result = cmd.spawn()?.wait()?;

        process::exit(result.code().unwrap_or(1));
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let workspace_info = WorkspaceInfo::from_args(&args)?;

    match args.command {
        Command::Test {
            group,
            features,
            options,
        } => workspace_info.execute_on_group("test", &group, features, options, false)?,
        Command::Build {
            group,
            features,
            options,
        } => workspace_info.execute_on_group("build", &group, features, options, true)?,
        Command::Check {
            group,
            features,
            options,
        } => workspace_info.execute_on_group("check", &group, features, options, true)?,
        Command::Clippy {
            group,
            features,
            options,
        } => workspace_info.execute_on_group("clippy", &group, features, options, true)?,
        Command::List { group: None } => workspace_info.print_groups()?,
        Command::List { group: Some(group) } => workspace_info.print_group(&group)?,
    };

    Ok(())
}
