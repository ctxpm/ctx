//! Turns the packages a project asks for into one concrete version of each package it
//! needs, following dependencies.
//!
//! Every package is installed at exactly one version. For each name the highest version
//! that satisfies every range on it wins; a version pinned in the lockfile is kept as
//! long as it still fits. Ranges that cannot be satisfied together are an error.
//!
//! The search is greedy: it never revisits a choice that still fits, so in rare graphs a
//! solution that needs an earlier package to move down is reported as a conflict.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use semver::{Version, VersionReq};

use crate::error::{Result, err};
use crate::range;
use crate::registry::VersionInfo;

/// Where the resolver gets the versions of a package from.
pub trait Source {
    /// All versions of `name`, or `None` when the package does not exist or the caller is
    /// not allowed to see it.
    fn versions(&mut self, name: &str) -> Result<Option<Vec<VersionInfo>>>;
}

#[derive(Debug, Default)]
pub struct Options {
    /// Versions to keep when they still satisfy every range (from the lockfile).
    pub locked: BTreeMap<String, Version>,
    /// Fail when a dependency cannot be seen, instead of leaving it out.
    pub strict: bool,
}

/// A dependency that was left out because the caller cannot see it.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub name: String,
    pub required_by: String,
}

#[derive(Debug, Default)]
pub struct Resolution {
    pub packages: BTreeMap<String, VersionInfo>,
    pub skipped: Vec<Skipped>,
}

struct Constraint {
    range: VersionReq,
    raw: String,
    /// "name@version" of the package that asks for it, or `None` for the project itself.
    required_by: Option<String>,
}

fn describe(constraints: &[Constraint]) -> String {
    constraints
        .iter()
        .map(|constraint| {
            format!(
                "\"{}\" (required by {})",
                constraint.raw,
                constraint.required_by.as_deref().unwrap_or("this project")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn choose<'a>(
    versions: &'a [VersionInfo],
    constraints: &[Constraint],
    locked: Option<&Version>,
) -> Option<&'a VersionInfo> {
    let fits = |candidate: &VersionInfo| {
        constraints
            .iter()
            .all(|constraint| range::matches(&constraint.range, &candidate.version))
    };
    if let Some(locked) = locked {
        // A pinned version stays installable even after it was yanked.
        if let Some(candidate) = versions
            .iter()
            .find(|candidate| &candidate.version == locked && fits(candidate))
        {
            return Some(candidate);
        }
    }
    versions
        .iter()
        .filter(|candidate| !candidate.yanked && fits(candidate))
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// Upper bound on resolver steps; only a pathological dependency graph can reach it.
const MAX_STEPS: usize = 100_000;

/// Resolves `roots` (package name to range) against `source`.
pub fn resolve(
    roots: &BTreeMap<String, String>,
    source: &mut dyn Source,
    options: &Options,
) -> Result<Resolution> {
    let mut constraints: BTreeMap<String, Vec<Constraint>> = BTreeMap::new();
    let mut selected: BTreeMap<String, VersionInfo> = BTreeMap::new();
    let mut known: BTreeMap<String, Option<Vec<VersionInfo>>> = BTreeMap::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    // Names for which no version fits right now. A later change elsewhere in the graph can
    // lift the ranges that block them, so this only becomes an error at the end.
    let mut blocked: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    for (name, raw) in roots {
        constraints
            .entry(name.clone())
            .or_default()
            .push(Constraint {
                range: range::parse(raw)?,
                raw: raw.clone(),
                required_by: None,
            });
        queue.push_back(name.clone());
    }

    let mut steps = 0usize;
    while let Some(name) = queue.pop_front() {
        steps += 1;
        if steps > MAX_STEPS {
            return Err(err(
                "could not resolve the dependencies: the packages keep forcing each other to other versions",
            ));
        }

        let wanted = constraints
            .get(&name)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if wanted.is_empty() {
            // Nothing asks for this package any more.
            blocked.remove(&name);
            continue;
        }
        if let Some(current) = selected.get(&name) {
            if wanted
                .iter()
                .all(|constraint| range::matches(&constraint.range, &current.version))
            {
                blocked.remove(&name);
                continue;
            }
        }

        if !known.contains_key(&name) {
            let versions = source.versions(&name)?;
            known.insert(name.clone(), versions);
        }
        let Some(versions) = known.get(&name).and_then(Option::as_ref) else {
            let direct = wanted
                .iter()
                .any(|constraint| constraint.required_by.is_none());
            if direct || options.strict {
                return Err(err(format!(
                    "package \"{name}\" was not found in the registry, or you do not have access to it ({})",
                    describe(wanted)
                )));
            }
            for constraint in wanted {
                let entry = Skipped {
                    name: name.clone(),
                    required_by: constraint.required_by.clone().unwrap_or_default(),
                };
                if !skipped.contains(&entry) {
                    skipped.push(entry);
                }
            }
            continue;
        };

        let Some(pick) = choose(versions, wanted, options.locked.get(&name)) else {
            blocked.insert(name);
            continue;
        };
        let pick = pick.clone();
        blocked.remove(&name);

        // Replacing a version withdraws everything the old version asked for; otherwise its
        // ranges would keep constraining packages it no longer depends on.
        if let Some(previous) = selected.get(&name) {
            let previous_requirer = format!("{name}@{}", previous.version);
            for (dependency, wanted_by_others) in constraints.iter_mut() {
                let before = wanted_by_others.len();
                wanted_by_others.retain(|constraint| {
                    constraint.required_by.as_deref() != Some(previous_requirer.as_str())
                });
                if wanted_by_others.len() != before {
                    queue.push_back(dependency.clone());
                }
            }
            skipped.retain(|entry| entry.required_by != previous_requirer);
        }

        let requirer = format!("{name}@{}", pick.version);
        for (dependency, raw) in &pick.dependencies {
            constraints
                .entry(dependency.clone())
                .or_default()
                .push(Constraint {
                    range: range::parse(raw)
                        .map_err(|error| err(format!("{requirer}: {error}")))?,
                    raw: raw.clone(),
                    required_by: Some(requirer.clone()),
                });
            queue.push_back(dependency.clone());
        }
        selected.insert(name, pick);
    }

    // Keep only what is still reachable from the roots through the selected versions.
    let mut reachable: BTreeSet<String> = BTreeSet::new();
    let mut pending: Vec<String> = roots.keys().cloned().collect();
    while let Some(name) = pending.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        if let Some(package) = selected.get(&name) {
            pending.extend(package.dependencies.keys().cloned());
        }
    }

    if let Some(name) = blocked.iter().find(|name| reachable.contains(*name)) {
        let wanted = constraints.get(name).map(Vec::as_slice).unwrap_or_default();
        let mut available: Vec<String> = known
            .get(name)
            .and_then(Option::as_ref)
            .map(|versions| {
                versions
                    .iter()
                    .filter(|candidate| !candidate.yanked)
                    .map(|candidate| candidate.version.to_string())
                    .collect()
            })
            .unwrap_or_default();
        available.truncate(10);
        return Err(err(format!(
            "no version of \"{name}\" satisfies {}; available: {}",
            describe(wanted),
            if available.is_empty() {
                "none".to_string()
            } else {
                available.join(", ")
            }
        )));
    }

    selected.retain(|name, _| reachable.contains(name));
    skipped.retain(|entry| reachable.contains(&entry.name));

    Ok(Resolution {
        packages: selected,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        packages: BTreeMap<String, Vec<VersionInfo>>,
        asked: Vec<String>,
    }

    impl Fake {
        fn with(mut self, name: &str, version: &str, dependencies: &[(&str, &str)]) -> Self {
            self.packages
                .entry(name.to_string())
                .or_default()
                .push(info(name, version, dependencies, false));
            self
        }

        fn yanked(mut self, name: &str, version: &str) -> Self {
            self.packages
                .entry(name.to_string())
                .or_default()
                .push(info(name, version, &[], true));
            self
        }
    }

    impl Source for Fake {
        fn versions(&mut self, name: &str) -> Result<Option<Vec<VersionInfo>>> {
            self.asked.push(name.to_string());
            Ok(self.packages.get(name).cloned())
        }
    }

    fn info(name: &str, version: &str, dependencies: &[(&str, &str)], yanked: bool) -> VersionInfo {
        VersionInfo {
            name: name.to_string(),
            version: Version::parse(version).unwrap(),
            kind: "project".into(),
            description: String::new(),
            dependencies: dependencies
                .iter()
                .map(|(name, range)| (name.to_string(), range.to_string()))
                .collect(),
            integrity: format!("sha256-{name}-{version}"),
            artifact: format!("packages/{name}/{version}/x.tgz"),
            archive_prefix: String::new(),
            size: 1,
            yanked,
            published_at: None,
            published_by: None,
        }
    }

    fn roots(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, range)| (name.to_string(), range.to_string()))
            .collect()
    }

    fn versions(resolution: &Resolution) -> Vec<String> {
        resolution
            .packages
            .iter()
            .map(|(name, package)| format!("{name}@{}", package.version))
            .collect()
    }

    #[test]
    fn picks_the_highest_matching_version_and_follows_dependencies() {
        let mut source = Fake::default()
            .with("app", "1.0.0", &[("base", "^1.0.0")])
            .with("app", "1.1.0", &[("base", "^1.2.0")])
            .with("app", "2.0.0-rc.1", &[])
            .with("base", "1.1.0", &[])
            .with("base", "1.3.0", &[])
            .with("base", "2.0.0", &[]);
        let resolution =
            resolve(&roots(&[("app", "*")]), &mut source, &Options::default()).unwrap();
        assert_eq!(versions(&resolution), ["app@1.1.0", "base@1.3.0"]);
        assert!(resolution.skipped.is_empty());
    }

    #[test]
    fn asks_the_registry_once_per_package() {
        let mut source = Fake::default()
            .with("a", "1.0.0", &[("shared", "*")])
            .with("b", "1.0.0", &[("shared", "*")])
            .with("shared", "1.0.0", &[]);
        resolve(
            &roots(&[("a", "*"), ("b", "*")]),
            &mut source,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(
            source.asked.iter().filter(|name| *name == "shared").count(),
            1
        );
    }

    #[test]
    fn lowers_a_version_when_a_later_range_narrows_it() {
        let mut source = Fake::default()
            .with("a", "1.0.0", &[("shared", "*")])
            .with("b", "1.0.0", &[("shared", "<2.0.0")])
            .with("shared", "1.5.0", &[])
            .with("shared", "2.0.0", &[("extra", "*")])
            .with("extra", "1.0.0", &[]);
        let resolution = resolve(
            &roots(&[("a", "*"), ("b", "*")]),
            &mut source,
            &Options::default(),
        )
        .unwrap();
        // shared@2.0.0 was picked first and then replaced; its dependency must not linger.
        assert_eq!(
            versions(&resolution),
            ["a@1.0.0", "b@1.0.0", "shared@1.5.0"]
        );
    }

    #[test]
    fn a_replaced_version_withdraws_its_own_ranges() {
        // shared@2.0.0 is picked first and asks for sub ^2; then b narrows shared to 1.x,
        // which asks for sub ^1. The range of the replaced shared@2.0.0 must not linger.
        let mut source = Fake::default()
            .with("a", "1.0.0", &[("shared", "*")])
            .with("b", "1.0.0", &[("shared", "<2.0.0")])
            .with("shared", "1.5.0", &[("sub", "^1.0.0")])
            .with("shared", "2.0.0", &[("sub", "^2.0.0")])
            .with("sub", "1.2.0", &[])
            .with("sub", "2.1.0", &[]);
        let resolution = resolve(
            &roots(&[("a", "*"), ("b", "*")]),
            &mut source,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(
            versions(&resolution),
            ["a@1.0.0", "b@1.0.0", "shared@1.5.0", "sub@1.2.0"]
        );
    }

    #[test]
    fn a_conflict_that_a_later_replacement_lifts_is_not_an_error() {
        // "sub" is first asked for as ^2 (by shared@2.0.0) and ^1 (by c): no version fits.
        // Once b narrows shared to 1.x, the ^2 range is withdrawn and sub resolves.
        let mut source = Fake::default()
            .with("a", "1.0.0", &[("shared", "*")])
            .with("c", "1.0.0", &[("sub", "^1.0.0")])
            .with("z", "1.0.0", &[("shared", "<2.0.0")])
            .with("shared", "1.5.0", &[])
            .with("shared", "2.0.0", &[("sub", "^2.0.0")])
            .with("sub", "1.2.0", &[])
            .with("sub", "2.1.0", &[]);
        let resolution = resolve(
            &roots(&[("a", "*"), ("c", "*"), ("z", "*")]),
            &mut source,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(
            versions(&resolution),
            ["a@1.0.0", "c@1.0.0", "shared@1.5.0", "sub@1.2.0", "z@1.0.0"]
        );
    }

    #[test]
    fn a_dependency_skipped_for_a_replaced_version_is_not_reported() {
        let mut source = Fake::default()
            .with("a", "1.0.0", &[("shared", "*")])
            .with("b", "1.0.0", &[("shared", "<2.0.0")])
            .with("shared", "1.5.0", &[])
            .with("shared", "2.0.0", &[("hidden", "*")]);
        let resolution = resolve(
            &roots(&[("a", "*"), ("b", "*")]),
            &mut source,
            &Options::default(),
        )
        .unwrap();
        assert_eq!(
            versions(&resolution),
            ["a@1.0.0", "b@1.0.0", "shared@1.5.0"]
        );
        assert!(resolution.skipped.is_empty());
    }

    #[test]
    fn reports_ranges_that_cannot_be_satisfied_together() {
        let mut source = Fake::default()
            .with("a", "1.0.0", &[("shared", "^1.0.0")])
            .with("b", "1.0.0", &[("shared", "^2.0.0")])
            .with("shared", "1.0.0", &[])
            .with("shared", "2.0.0", &[]);
        let error = resolve(
            &roots(&[("a", "*"), ("b", "*")]),
            &mut source,
            &Options::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("no version of \"shared\""), "{error}");
        assert!(
            error.contains("required by a@1.0.0") && error.contains("required by b@1.0.0"),
            "{error}"
        );
        assert!(
            error.contains("available: 2.0.0, 1.0.0") || error.contains("available: 1.0.0, 2.0.0"),
            "{error}"
        );
    }

    #[test]
    fn keeps_a_locked_version_while_it_fits() {
        let source = || {
            Fake::default()
                .with("a", "1.0.0", &[])
                .with("a", "1.1.0", &[])
                .with("a", "2.0.0", &[])
        };
        let locked = |version: &str| Options {
            locked: BTreeMap::from([("a".to_string(), Version::parse(version).unwrap())]),
            strict: false,
        };

        let kept = resolve(&roots(&[("a", "^1.0.0")]), &mut source(), &locked("1.0.0")).unwrap();
        assert_eq!(versions(&kept), ["a@1.0.0"]);

        let moved = resolve(&roots(&[("a", "^2.0.0")]), &mut source(), &locked("1.0.0")).unwrap();
        assert_eq!(versions(&moved), ["a@2.0.0"]);
    }

    #[test]
    fn skips_yanked_versions_unless_locked() {
        let source = || Fake::default().with("a", "1.0.0", &[]).yanked("a", "1.1.0");
        let fresh = resolve(&roots(&[("a", "*")]), &mut source(), &Options::default()).unwrap();
        assert_eq!(versions(&fresh), ["a@1.0.0"]);

        let locked = Options {
            locked: BTreeMap::from([("a".to_string(), Version::new(1, 1, 0))]),
            strict: false,
        };
        let pinned = resolve(&roots(&[("a", "*")]), &mut source(), &locked).unwrap();
        assert_eq!(versions(&pinned), ["a@1.1.0"]);

        let only_yanked = resolve(
            &roots(&[("b", "*")]),
            &mut Fake::default().yanked("b", "1.0.0"),
            &Options::default(),
        );
        assert!(
            only_yanked
                .unwrap_err()
                .to_string()
                .contains("available: none")
        );
    }

    #[test]
    fn leaves_out_dependencies_the_caller_cannot_see() {
        let mut source = Fake::default()
            .with("app", "1.0.0", &[("private-base", "*"), ("base", "*")])
            .with("base", "1.0.0", &[]);
        let resolution =
            resolve(&roots(&[("app", "*")]), &mut source, &Options::default()).unwrap();
        assert_eq!(versions(&resolution), ["app@1.0.0", "base@1.0.0"]);
        assert_eq!(
            resolution.skipped,
            [Skipped {
                name: "private-base".into(),
                required_by: "app@1.0.0".into()
            }]
        );
    }

    #[test]
    fn strict_mode_and_direct_requests_fail_on_invisible_packages() {
        let source = || Fake::default().with("app", "1.0.0", &[("private-base", "*")]);
        let strict = Options {
            locked: BTreeMap::new(),
            strict: true,
        };
        assert!(
            resolve(&roots(&[("app", "*")]), &mut source(), &strict)
                .unwrap_err()
                .to_string()
                .contains("private-base")
        );

        let direct = resolve(
            &roots(&[("missing", "*")]),
            &mut source(),
            &Options::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            direct.contains("\"missing\" was not found") && direct.contains("this project"),
            "{direct}"
        );
    }

    #[test]
    fn handles_dependency_cycles() {
        let mut source =
            Fake::default()
                .with("a", "1.0.0", &[("b", "*")])
                .with("b", "1.0.0", &[("a", "*")]);
        let resolution = resolve(&roots(&[("a", "*")]), &mut source, &Options::default()).unwrap();
        assert_eq!(versions(&resolution), ["a@1.0.0", "b@1.0.0"]);
    }

    #[test]
    fn rejects_an_invalid_root_range() {
        let error = resolve(
            &roots(&[("a", "^1 || ^2")]),
            &mut Fake::default(),
            &Options::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not support"));
    }
}
