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
use tracing::info;
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
    },
    /// Build a group of crates
    #[command(override_usage = "Usage: cargo groups build [OPTIONS] <GROUP>")]
    Build {
        group: String,
        #[command(flatten)]
        features: clap_cargo::Features,
    },
    /// Check a group of crates
    #[command(override_usage = "Usage: cargo groups check [OPTIONS] <GROUP>")]
    Check {
        group: String,
        #[command(flatten)]
        features: clap_cargo::Features,
    },
    /// Run clippy on a group of crates
    #[command(override_usage = "Usage: cargo groups clippy [OPTIONS] <GROUP>")]
    Clippy {
        group: String,
        #[command(flatten)]
        features: clap_cargo::Features,
    },
    /// List the groups in the workspace. Add a group name to list the crates in that specific group
    #[command(override_usage = "Usage: cargo groups list [GROUP]")]
    List { group: Option<String> },
}

impl CargoToml {
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
        Ok(toml::from_str::<CargoToml>(&cargo_toml_contents)?)
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
    cargo_toml: CargoToml,
}

impl WorkspaceInfo {
    fn from_args(args: &Args) -> Result<Self> {
        let cwd = args.cwd.clone().unwrap_or_else(|| current_dir().unwrap());
        let cargo_toml_path = CargoToml::find(&cwd, &args.manifest.manifest_path)?;
        let metadata = MetadataCommand::new()
            .manifest_path(&cargo_toml_path)
            .exec()?;
        let cargo_toml = CargoToml::load(&cargo_toml_path)?;

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
            for package in self.get_group_crates(&crates)? {
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
        for package in self.get_group_crates(crates)? {
            self.print_package(package);
        }

        Ok(())
    }

    fn get_group_crates(
        &self,
        group_patterns: &[String],
    ) -> Result<impl Iterator<Item = &Package>> {
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

        Ok(self
            .metadata
            .workspace_packages()
            .into_iter()
            .filter(move |package| {
                crates_by_package.is_match(&package.name)
                    || crates_by_path.is_match(self.get_package_path_relative_to_workspace(package))
            }))
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

    fn execute_on_group(
        &self,
        subcommand: &str,
        group: &str,
        features: clap_cargo::Features,
    ) -> Result<()> {
        let Some(crates) = self.cargo_toml.workspace.metadata.groups.get(group) else {
            return Err(anyhow::anyhow!("Group {} not found", group));
        };

        let cargo = which("cargo")?;
        let mut cmd = process::Command::new(cargo);
        cmd.current_dir(&self.cwd).arg(subcommand);
        add_features(&mut cmd, &features);

        for member in self.get_group_crates(crates)? {
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
        Command::Test { group, features } => {
            workspace_info.execute_on_group("test", &group, features)?
        }
        Command::Build { group, features } => {
            workspace_info.execute_on_group("build", &group, features)?
        }
        Command::Check { group, features } => {
            workspace_info.execute_on_group("check", &group, features)?
        }
        Command::Clippy { group, features } => {
            workspace_info.execute_on_group("clippy", &group, features)?
        }
        Command::List { group: None } => workspace_info.print_groups()?,
        Command::List { group: Some(group) } => workspace_info.print_group(&group)?,
    };

    Ok(())
}
