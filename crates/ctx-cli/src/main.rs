//! `ctx` — command-line front end of the ctx package manager.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use ctx_core::config::{Config, Credentials, RegistryEntry};
use ctx_core::descriptor::{Descriptor, WELL_KNOWN_PATH};
use ctx_core::error::{Context, err};
use ctx_core::install::{self, Target};
use ctx_core::lockfile::{LockedPackage, Lockfile};
use ctx_core::manifest::is_package_name;
use ctx_core::project::Project;
use ctx_core::registry::{Registry, VersionInfo};
use ctx_core::reqwest::blocking::Client;
use ctx_core::resolve::{self, Options, Source};
use ctx_core::{Result, archive, auth, range};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(
    name = "ctx",
    version,
    about = "Package manager for context artifacts",
    propagate_version = true
)]
struct Cli {
    /// Registry to use, when more than one is configured
    #[arg(long, global = true, value_name = "NAME")]
    registry: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage the registries ctx knows about
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },
    /// Sign in to the registry with your Google account
    Login {
        /// Sign in to an emulated registry as this address (development only)
        #[arg(long, value_name = "EMAIL", hide = true)]
        dev_email: Option<String>,
    },
    /// Forget the sign-in session for the registry
    Logout,
    /// Show who is signed in
    Whoami,
    /// Install packages, or everything listed in ctx.toml
    Install(InstallArgs),
    /// Move installed packages to the newest versions their ranges allow
    Update {
        /// Packages to update; all when omitted
        packages: Vec<String>,
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Remove packages from this project
    Uninstall {
        #[arg(required = true)]
        packages: Vec<String>,
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// List the packages installed in this project
    List {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Show installed packages that have a newer version
    Outdated {
        #[command(flatten)]
        project: ProjectArgs,
    },
    /// Show a package and its versions
    Info { package: String },
    /// List the packages you can read, optionally filtered
    Search { query: Option<String> },
    /// Build a package archive from a directory
    Pack {
        dir: PathBuf,
        /// Where to write the archive (default: <name>-<version>.tgz)
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Publish a package directory or archive
    Publish {
        path: PathBuf,
        /// Publish under this dist-tag instead of moving "latest"
        #[arg(long, value_name = "TAG")]
        tag: Option<String>,
    },
    /// Claim a package name and set who can read it, before the first publish
    Create(CreateArgs),
    /// Mark a version as yanked, so ranges no longer select it
    Yank {
        /// name@version
        spec: String,
        /// Make the version selectable again
        #[arg(long)]
        undo: bool,
    },
    /// Show or change who can read and maintain a package
    Access {
        #[command(subcommand)]
        command: AccessCommand,
    },
}

#[derive(Subcommand)]
enum RegistryCommand {
    /// Add a registry by its address, or by the path of a descriptor file
    Add {
        source: String,
        /// Name to refer to this registry by
        #[arg(long)]
        name: Option<String>,
    },
    /// List configured registries
    List,
    /// Make a registry the default
    Use { name: String },
    /// Forget a registry and its sign-in session
    Remove { name: String },
}

#[derive(Subcommand)]
enum AccessCommand {
    /// Show the access lists of a package
    List { package: String },
    /// Give read access to email addresses or, written as @example.dev, to a whole domain
    Grant {
        package: String,
        #[arg(required = true)]
        who: Vec<String>,
        /// Make them maintainers instead of readers
        #[arg(long)]
        maintainer: bool,
    },
    /// Take access away again
    Revoke {
        package: String,
        #[arg(required = true)]
        who: Vec<String>,
        #[arg(long)]
        maintainer: bool,
    },
    /// Make a package public or restricted
    Visibility {
        package: String,
        #[arg(value_parser = ["public", "restricted"])]
        visibility: String,
    },
}

#[derive(Args)]
struct ProjectArgs {
    /// Project directory (default: the current directory)
    #[arg(long, value_name = "DIR", default_value = ".")]
    dir: PathBuf,
}

#[derive(Args)]
struct InstallArgs {
    /// Packages to add, as name or name@range
    packages: Vec<String>,
    #[command(flatten)]
    project: ProjectArgs,
    /// Fail when a dependency cannot be read instead of leaving it out
    #[arg(long)]
    strict: bool,
    /// Install exactly what ctx.lock says and change nothing
    #[arg(long)]
    frozen: bool,
}

#[derive(Args)]
struct CreateArgs {
    name: String,
    /// Kind of context, for example "project"
    #[arg(long = "type", value_name = "TYPE")]
    kind: String,
    #[arg(long, default_value = "")]
    description: String,
    /// Readable by anyone instead of only the people you grant access
    #[arg(long)]
    public: bool,
    /// Email address or @domain that may read the package (repeatable)
    #[arg(long = "reader", value_name = "WHO")]
    readers: Vec<String>,
    /// Email address of an additional maintainer (repeatable)
    #[arg(long = "maintainer", value_name = "EMAIL")]
    maintainers: Vec<String>,
    /// GitHub repository (owner/repo) whose workflows may publish (repeatable)
    #[arg(long = "trust", value_name = "OWNER/REPO")]
    trusted: Vec<String>,
}

fn http() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!("ctx/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(120))
        .build()?)
}

/// The token to present to the registry: `CTX_TOKEN` when set (for CI), else the stored
/// session, refreshed when it is about to expire. `None` means anonymous.
fn token_for(http: &Client, name: &str, descriptor: &Descriptor) -> Result<Option<String>> {
    if let Some(token) = std::env::var("CTX_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(token));
    }
    let mut credentials = Credentials::load()?;
    let Some(session) = credentials.sessions.get(name).cloned() else {
        return Ok(None);
    };
    if auth::is_fresh(&session) {
        return Ok(Some(session.id_token));
    }
    let refreshed = auth::refresh(http, descriptor, &session)?;
    let token = refreshed.id_token.clone();
    credentials.sessions.insert(name.to_string(), refreshed);
    credentials.save()?;
    Ok(Some(token))
}

/// Registry address compiled into release builds; used when none was added yet.
const DEFAULT_REGISTRY: Option<&str> = option_env!("CTX_DEFAULT_REGISTRY");

fn fetch_descriptor(source: &str) -> Result<Descriptor> {
    let is_url = source.starts_with("https://") || source.starts_with("http://");
    let raw = if is_url {
        let url = if source.ends_with(".json") {
            source.to_string()
        } else {
            format!("{}{WELL_KNOWN_PATH}", source.trim_end_matches('/'))
        };
        let response = http()?.get(&url).send()?;
        if !response.status().is_success() {
            return Err(err(format!(
                "{url} returned {}; is this a ctx registry?",
                response.status()
            )));
        }
        response.text()?
    } else {
        fs::read_to_string(source).context(format!("cannot read {source}"))?
    };
    Descriptor::parse(&raw)
}

/// Loads the configuration; on first use of a release build, adds its default registry.
fn load_config() -> Result<Config> {
    let mut config = Config::load()?;
    if config.registries.is_empty() {
        if let Some(url) = DEFAULT_REGISTRY.filter(|url| !url.is_empty()) {
            let descriptor = fetch_descriptor(url)?;
            let name = descriptor
                .name
                .as_deref()
                .map(slug)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "default".into());
            eprintln!("Using the registry at {url} (change it with `ctx registry add`).");
            config.registries.insert(
                name.clone(),
                RegistryEntry {
                    source: url.to_string(),
                    descriptor,
                },
            );
            config.default_registry = Some(name);
            config.save()?;
        }
    }
    Ok(config)
}

fn open_registry(selected: Option<&str>) -> Result<Registry> {
    let config = load_config()?;
    let (name, entry) = config.registry(selected)?;
    let http = http()?;
    let token = token_for(&http, &name, &entry.descriptor)?;
    Ok(Registry::new(http, entry.descriptor.clone(), token))
}

/// Splits "name" or "name@range".
fn parse_spec(spec: &str) -> Result<(String, Option<String>)> {
    let (name, wanted) = match spec.split_once('@') {
        Some((name, wanted)) => (name, Some(wanted.to_string())),
        None => (spec, None),
    };
    if !is_package_name(name) {
        return Err(err(format!("\"{name}\" is not a valid package name")));
    }
    if let Some(wanted) = &wanted {
        range::parse(wanted)?;
    }
    Ok((name.to_string(), wanted))
}

struct RegistrySource<'a>(&'a Registry);

impl Source for RegistrySource<'_> {
    fn versions(&mut self, name: &str) -> Result<Option<Vec<VersionInfo>>> {
        self.0.versions(name)
    }
}

fn target_of(project: &Project, dir: &Path) -> Target {
    Target {
        packages_dir: project.packages_dir(dir),
        preserve: project.install.preserve.clone(),
    }
}

/// Brings the packages directory in line with `wanted`, writes the lockfile and reports
/// what changed.
fn apply(
    registry: &Registry,
    dir: &Path,
    project: &Project,
    previous: &Lockfile,
    wanted: Lockfile,
) -> Result<()> {
    let target = target_of(project, dir);
    let mut changed = 0usize;

    for (name, package) in &wanted.packages {
        let installed = install::installed_version(&target, name);
        if installed.as_deref() == Some(package.version.as_str()) {
            continue;
        }
        let bytes = registry.download(&package.artifact)?;
        install::install(
            &target,
            name,
            &package.version,
            &bytes,
            &package.integrity,
            &package.archive_prefix,
        )?;
        match installed {
            Some(old) => println!("  {name} {old} -> {}", package.version),
            None => println!("+ {name} {}", package.version),
        }
        changed += 1;
    }

    for name in previous
        .packages
        .keys()
        .filter(|name| !wanted.packages.contains_key(*name))
    {
        if install::uninstall(&target, name)? {
            println!("- {name}");
        } else {
            println!(
                "- {name} (kept local files in {})",
                target.packages_dir.join(name).display()
            );
        }
        changed += 1;
    }

    wanted.save(dir)?;
    if changed == 0 {
        println!(
            "Already up to date ({} package{}).",
            wanted.packages.len(),
            if wanted.packages.len() == 1 { "" } else { "s" }
        );
    } else {
        println!(
            "{} package{} in {}",
            wanted.packages.len(),
            if wanted.packages.len() == 1 { "" } else { "s" },
            target.packages_dir.display()
        );
    }
    Ok(())
}

/// Resolves the project's packages and installs the result. `unlock` names the packages
/// whose pinned versions may move; `None` keeps every pin that still fits.
fn sync(
    registry: &Registry,
    dir: &Path,
    project: &Project,
    strict: bool,
    unlock: Option<&BTreeSet<String>>,
) -> Result<Lockfile> {
    let previous = Lockfile::load(dir)?;
    let mut locked = previous.versions();
    if let Some(unlock) = unlock {
        if unlock.is_empty() {
            locked.clear();
        } else {
            locked.retain(|name, _| !unlock.contains(name));
        }
    }
    let resolution = resolve::resolve(
        &project.packages,
        &mut RegistrySource(registry),
        &Options { locked, strict },
    )?;
    for skipped in &resolution.skipped {
        eprintln!(
            "warning: left out \"{}\" (needed by {}): it does not exist or you do not have access to it",
            skipped.name, skipped.required_by
        );
    }
    let wanted = Lockfile {
        lockfile_version: 1,
        packages: resolution
            .packages
            .iter()
            .map(|(name, info)| (name.clone(), LockedPackage::from(info)))
            .collect(),
    };
    apply(registry, dir, project, &previous, wanted.clone())?;
    Ok(wanted)
}

fn cmd_install(registry_name: Option<&str>, args: InstallArgs) -> Result<()> {
    let dir = args.project.dir;
    let mut project = Project::load(&dir)?;
    let registry = open_registry(registry_name)?;

    if args.frozen {
        if !args.packages.is_empty() {
            return Err(err(
                "--frozen installs what ctx.lock lists and cannot add packages",
            ));
        }
        let lockfile = Lockfile::load(&dir)?;
        if lockfile.packages.is_empty() {
            return Err(err(format!(
                "--frozen needs a ctx.lock in {}",
                dir.display()
            )));
        }
        // With no "previous" state nothing is removed; frozen installs only add or replace.
        return apply(&registry, &dir, &project, &lockfile, lockfile.clone());
    }

    let mut unpinned: Vec<String> = Vec::new();
    for spec in &args.packages {
        let (name, wanted) = parse_spec(spec)?;
        match wanted {
            Some(wanted) => {
                project.packages.insert(name, wanted);
            }
            None => {
                // Resolve against anything first; the range is written down afterwards.
                if !project.packages.contains_key(&name) {
                    project.packages.insert(name.clone(), "*".into());
                    unpinned.push(name);
                }
            }
        }
    }
    if project.packages.is_empty() {
        println!(
            "Nothing to install: {} lists no packages. Try `ctx install <package>`.",
            dir.join("ctx.toml").display()
        );
        return Ok(());
    }
    fs::create_dir_all(&dir).context(format!("cannot create {}", dir.display()))?;

    let lockfile = sync(&registry, &dir, &project, args.strict, None)?;
    for name in unpinned {
        if let Some(package) = lockfile.packages.get(&name) {
            project
                .packages
                .insert(name, format!("^{}", package.version));
        }
    }
    if !args.packages.is_empty() {
        project.save(&dir)?;
    }
    Ok(())
}

fn cmd_update(registry_name: Option<&str>, packages: Vec<String>, dir: PathBuf) -> Result<()> {
    let project = Project::load(&dir)?;
    if project.packages.is_empty() {
        return Err(err(format!(
            "{} lists no packages",
            dir.join("ctx.toml").display()
        )));
    }
    let lockfile = Lockfile::load(&dir)?;
    let mut unlock = BTreeSet::new();
    for spec in &packages {
        let (name, _) = parse_spec(spec)?;
        if !lockfile.packages.contains_key(&name) && !project.packages.contains_key(&name) {
            return Err(err(format!("\"{name}\" is not installed in this project")));
        }
        unlock.insert(name);
    }
    let registry = open_registry(registry_name)?;
    sync(&registry, &dir, &project, false, Some(&unlock))?;
    Ok(())
}

fn cmd_uninstall(registry_name: Option<&str>, packages: Vec<String>, dir: PathBuf) -> Result<()> {
    let mut project = Project::load(&dir)?;
    for spec in &packages {
        let (name, _) = parse_spec(spec)?;
        if project.packages.remove(&name).is_none() {
            return Err(err(format!(
                "\"{name}\" is not listed in ctx.toml; packages that are only dependencies go away with what needs them"
            )));
        }
    }
    project.save(&dir)?;
    if project.packages.is_empty() {
        // Nothing left to resolve, so no registry is needed.
        let previous = Lockfile::load(&dir)?;
        let target = target_of(&project, &dir);
        for name in previous.packages.keys() {
            install::uninstall(&target, name)?;
            println!("- {name}");
        }
        return Lockfile::default().save(&dir);
    }
    let registry = open_registry(registry_name)?;
    sync(&registry, &dir, &project, false, None)?;
    Ok(())
}

fn cmd_list(dir: PathBuf) -> Result<()> {
    let project = Project::load(&dir)?;
    let lockfile = Lockfile::load(&dir)?;
    let target = target_of(&project, &dir);
    if lockfile.packages.is_empty() {
        println!("No packages installed in {}.", dir.display());
        return Ok(());
    }
    for (name, package) in &lockfile.packages {
        let state = match install::installed_version(&target, name) {
            Some(version) if version == package.version => String::new(),
            Some(version) => format!("  (on disk: {version}; run `ctx install`)"),
            None => "  (not on disk; run `ctx install`)".to_string(),
        };
        let role = if project.packages.contains_key(name) {
            ""
        } else {
            "  (dependency)"
        };
        println!("{name} {} [{}]{role}{state}", package.version, package.kind);
    }
    Ok(())
}

fn cmd_outdated(registry_name: Option<&str>, dir: PathBuf) -> Result<()> {
    let lockfile = Lockfile::load(&dir)?;
    let registry = open_registry(registry_name)?;
    let mut any = false;
    for (name, package) in &lockfile.packages {
        let Some(versions) = registry.versions(name)? else {
            continue;
        };
        let newest = versions
            .iter()
            .filter(|info| !info.yanked && info.version.pre.is_empty())
            .map(|info| &info.version)
            .max();
        if let Some(newest) = newest {
            if newest.to_string() != package.version
                && semver::Version::parse(&package.version).is_ok_and(|current| newest > &current)
            {
                println!("{name} {} -> {newest}", package.version);
                any = true;
            }
        }
    }
    if !any {
        println!("Everything is on its newest version.");
    }
    Ok(())
}

fn cmd_info(registry_name: Option<&str>, name: &str) -> Result<()> {
    let registry = open_registry(registry_name)?;
    let package = registry.package(name)?.ok_or_else(|| {
        err(format!(
            "package \"{name}\" was not found, or you do not have access to it"
        ))
    })?;
    println!("{} [{}] {}", package.name, package.kind, package.visibility);
    if !package.description.is_empty() {
        println!("{}", package.description);
    }
    for (tag, version) in &package.dist_tags {
        println!("{tag}: {version}");
    }
    println!("maintainers: {}", package.maintainers.join(", "));
    println!("versions:");
    for info in registry.versions(name)?.unwrap_or_default() {
        let dependencies = if info.dependencies.is_empty() {
            String::new()
        } else {
            format!(
                "  needs {}",
                info.dependencies
                    .iter()
                    .map(|(name, wanted)| format!("{name}@{wanted}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        println!(
            "  {}{}{}{dependencies}",
            info.version,
            if info.yanked { "  (yanked)" } else { "" },
            info.published_by
                .as_deref()
                .map(|by| format!("  by {by}"))
                .unwrap_or_default(),
        );
    }
    Ok(())
}

fn cmd_search(registry_name: Option<&str>, query: Option<String>) -> Result<()> {
    let registry = open_registry(registry_name)?;
    let needle = query.unwrap_or_default().to_lowercase();
    let packages: Vec<_> = registry
        .packages()?
        .into_iter()
        .filter(|package| {
            needle.is_empty()
                || package.name.contains(&needle)
                || package.description.to_lowercase().contains(&needle)
        })
        .collect();
    if packages.is_empty() {
        println!(
            "No packages found{}.",
            if registry.is_signed_in() {
                ""
            } else {
                " (you are not signed in; only public packages are visible)"
            }
        );
    }
    for package in packages {
        println!(
            "{} {} [{}] {}  {}",
            package.name,
            package.latest.as_deref().unwrap_or("-"),
            package.kind,
            package.visibility,
            package.description
        );
    }
    Ok(())
}

fn cmd_pack(dir: &Path, output: Option<PathBuf>) -> Result<()> {
    let (manifest, bytes) = archive::pack(dir)?;
    let output = output
        .unwrap_or_else(|| PathBuf::from(format!("{}-{}.tgz", manifest.name, manifest.version)));
    fs::write(&output, &bytes).context(format!("cannot write {}", output.display()))?;
    println!(
        "{} ({} bytes, {})",
        output.display(),
        bytes.len(),
        archive::integrity(&bytes)
    );
    Ok(())
}

fn cmd_publish(registry_name: Option<&str>, path: &Path, tag: Option<&str>) -> Result<()> {
    let bytes = if path.is_dir() {
        archive::pack(path)?.1
    } else {
        fs::read(path).context(format!("cannot read {}", path.display()))?
    };
    let registry = open_registry(registry_name)?;
    let result = registry.publish(bytes, tag)?;
    let text = |key: &str| {
        result
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string()
    };
    println!(
        "Published {}@{} ({})",
        text("name"),
        text("version"),
        text("integrity")
    );
    Ok(())
}

/// Splits a mixed list into email addresses and "@domain" entries, lower-cased.
fn split_who(who: &[String]) -> (Vec<String>, Vec<String>) {
    let mut emails = Vec::new();
    let mut domains = Vec::new();
    for entry in who {
        let entry = entry.trim().to_lowercase();
        match entry.strip_prefix('@') {
            Some(domain) => domains.push(domain.to_string()),
            None => emails.push(entry),
        }
    }
    (emails, domains)
}

fn cmd_create(registry_name: Option<&str>, args: CreateArgs) -> Result<()> {
    let (readers, reader_domains) = split_who(&args.readers);
    let registry = open_registry(registry_name)?;
    registry.create_package(&json!({
        "name": args.name,
        "type": args.kind,
        "description": args.description,
        "visibility": if args.public { "public" } else { "restricted" },
        "readers": readers,
        "readerDomains": reader_domains,
        "maintainers": args.maintainers,
        "trustedPublishers": args.trusted.iter().map(|repository| json!({ "provider": "github", "repository": repository })).collect::<Vec<_>>(),
    }))?;
    println!("Created {}", args.name);
    Ok(())
}

fn cmd_yank(registry_name: Option<&str>, spec: &str, undo: bool) -> Result<()> {
    let (name, version) = spec
        .split_once('@')
        .filter(|(name, version)| is_package_name(name) && semver::Version::parse(version).is_ok())
        .ok_or_else(|| err("expected name@version, for example team-handbook@1.2.0"))?;
    let registry = open_registry(registry_name)?;
    registry.yank(name, version, !undo)?;
    println!(
        "{} {name}@{version}",
        if undo { "Restored" } else { "Yanked" }
    );
    Ok(())
}

/// Adds people or domains to the access lists of a package, or removes them.
fn change_access(
    registry: &Registry,
    info: &ctx_core::registry::PackageInfo,
    who: &[String],
    maintainer: bool,
    granting: bool,
) -> Result<()> {
    let (emails, domains) = split_who(who);
    if maintainer && !domains.is_empty() {
        return Err(err(
            "a whole domain cannot be a maintainer; name the people",
        ));
    }
    let change = |current: &[String], entries: &[String]| -> Vec<String> {
        let mut set: BTreeSet<String> = current.iter().cloned().collect();
        for entry in entries {
            if granting {
                set.insert(entry.clone());
            } else {
                set.remove(entry);
            }
        }
        set.into_iter().collect()
    };
    let mut update: BTreeMap<&str, Value> = BTreeMap::new();
    if maintainer {
        let maintainers = change(&info.maintainers, &emails);
        if maintainers.is_empty() {
            return Err(err("a package needs at least one maintainer"));
        }
        update.insert("maintainers", json!(maintainers));
    } else {
        if !emails.is_empty() {
            update.insert("readers", json!(change(&info.readers, &emails)));
        }
        if !domains.is_empty() {
            update.insert(
                "readerDomains",
                json!(change(&info.reader_domains, &domains)),
            );
        }
    }
    registry.set_access(&info.name, &update)
}

fn cmd_access(registry_name: Option<&str>, command: AccessCommand) -> Result<()> {
    let registry = open_registry(registry_name)?;
    let load = |name: &str| {
        registry.package(name)?.ok_or_else(|| {
            err(format!(
                "package \"{name}\" was not found, or you do not have access to it"
            ))
        })
    };
    let list = |items: &[String]| {
        if items.is_empty() {
            "-".to_string()
        } else {
            items.join(", ")
        }
    };
    match command {
        AccessCommand::List { package } => {
            let info = load(&package)?;
            println!("visibility:   {}", info.visibility);
            println!("maintainers:  {}", list(&info.maintainers));
            println!("readers:      {}", list(&info.readers));
            println!("domains:      {}", list(&info.reader_domains));
        }
        AccessCommand::Visibility {
            package,
            visibility,
        } => {
            registry.set_access(
                &package,
                &BTreeMap::from([("visibility", json!(visibility))]),
            )?;
            println!("{package} is now {visibility}");
        }
        AccessCommand::Grant {
            package,
            who,
            maintainer,
        } => {
            change_access(&registry, &load(&package)?, &who, maintainer, true)?;
            println!("Updated access to {package}");
        }
        AccessCommand::Revoke {
            package,
            who,
            maintainer,
        } => {
            change_access(&registry, &load(&package)?, &who, maintainer, false)?;
            println!("Updated access to {package}");
        }
    }
    Ok(())
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn cmd_registry(command: RegistryCommand) -> Result<()> {
    let mut config = Config::load()?;
    match command {
        RegistryCommand::Add { source, name } => {
            let descriptor = fetch_descriptor(&source)?;
            let name = name
                .or_else(|| descriptor.name.as_deref().map(slug))
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| slug(&descriptor.firebase.project_id));
            config
                .registries
                .insert(name.clone(), RegistryEntry { source, descriptor });
            if config.default_registry.is_none() {
                config.default_registry = Some(name.clone());
            }
            config.save()?;
            println!(
                "Added registry \"{name}\"{}",
                if config.default_registry.as_deref() == Some(&name) {
                    " (default)"
                } else {
                    ""
                }
            );
        }
        RegistryCommand::List => {
            if config.registries.is_empty() {
                println!("No registries configured. Add one with `ctx registry add <url>`.");
            }
            for (name, entry) in &config.registries {
                let marker = if config.default_registry.as_deref() == Some(name) {
                    "*"
                } else {
                    " "
                };
                println!("{marker} {name}  {}", entry.source);
            }
        }
        RegistryCommand::Use { name } => {
            if !config.registries.contains_key(&name) {
                return Err(err(format!("unknown registry \"{name}\"")));
            }
            config.default_registry = Some(name.clone());
            config.save()?;
            println!("Default registry is now \"{name}\"");
        }
        RegistryCommand::Remove { name } => {
            if config.registries.remove(&name).is_none() {
                return Err(err(format!("unknown registry \"{name}\"")));
            }
            if config.default_registry.as_deref() == Some(&name) {
                config.default_registry = None;
            }
            config.save()?;
            let mut credentials = Credentials::load()?;
            if credentials.sessions.remove(&name).is_some() {
                credentials.save()?;
            }
            println!("Removed registry \"{name}\"");
        }
    }
    Ok(())
}

fn cmd_login(registry_name: Option<&str>, dev_email: Option<String>) -> Result<()> {
    let config = load_config()?;
    let (name, entry) = config.registry(registry_name)?;
    let http = http()?;
    let session = match dev_email {
        Some(email) => auth::sign_in_emulated(&http, &entry.descriptor, &email)?,
        None => auth::sign_in_with_browser(&http, &entry.descriptor, |line| eprintln!("{line}"))?,
    };
    let email = session.email.clone();
    let mut credentials = Credentials::load()?;
    credentials.sessions.insert(name.clone(), session);
    credentials.save()?;
    println!("Signed in to \"{name}\" as {email}");
    Ok(())
}

fn cmd_logout(registry_name: Option<&str>) -> Result<()> {
    let config = load_config()?;
    let (name, _) = config.registry(registry_name)?;
    let mut credentials = Credentials::load()?;
    if credentials.sessions.remove(&name).is_some() {
        credentials.save()?;
        println!("Signed out of \"{name}\"");
    } else {
        println!("You were not signed in to \"{name}\"");
    }
    Ok(())
}

fn cmd_whoami(registry_name: Option<&str>) -> Result<()> {
    let config = load_config()?;
    let (name, entry) = config.registry(registry_name)?;
    match token_for(&http()?, &name, &entry.descriptor)? {
        Some(token) => match auth::email_of(&token) {
            Some(email) => println!("{email} (registry \"{name}\")"),
            None => println!(
                "Signed in to \"{name}\" with a token that carries no email address (a CI token)"
            ),
        },
        None => println!("Not signed in to \"{name}\". Run `ctx login`."),
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    let registry = cli.registry.as_deref();
    match cli.command {
        Command::Registry { command } => cmd_registry(command),
        Command::Login { dev_email } => cmd_login(registry, dev_email),
        Command::Logout => cmd_logout(registry),
        Command::Whoami => cmd_whoami(registry),
        Command::Install(args) => cmd_install(registry, args),
        Command::Update { packages, project } => cmd_update(registry, packages, project.dir),
        Command::Uninstall { packages, project } => cmd_uninstall(registry, packages, project.dir),
        Command::List { project } => cmd_list(project.dir),
        Command::Outdated { project } => cmd_outdated(registry, project.dir),
        Command::Info { package } => cmd_info(registry, &package),
        Command::Search { query } => cmd_search(registry, query),
        Command::Pack { dir, output } => cmd_pack(&dir, output),
        Command::Publish { path, tag } => cmd_publish(registry, &path, tag.as_deref()),
        Command::Create(args) => cmd_create(registry, args),
        Command::Yank { spec, undo } => cmd_yank(registry, &spec, undo),
        Command::Access { command } => cmd_access(registry, command),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
