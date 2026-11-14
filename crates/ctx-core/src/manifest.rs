//! `manifest.json`, the file at the root of every package. See `spec/manifest.schema.json`.

use std::collections::BTreeMap;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::{Result, err};
use crate::range;

pub const MANIFEST_FILE: &str = "manifest.json";

/// The fields of a manifest that ctx itself uses. Other fields are left alone.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Manifest {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub name: String,
    pub version: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Free-form sections; ctx only checks that they are objects, like the registry does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provides: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engines: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<serde_json::Value>,
}

const MAX_DEPENDENCIES: usize = 200;

/// Lowercase kebab-case, at most 64 characters.
pub fn is_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

fn is_package_type(kind: &str) -> bool {
    let mut chars = kind.chars();
    kind.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Manifest {
    /// Parses and validates the text of a manifest.json.
    pub fn parse(raw: &str) -> Result<Manifest> {
        let manifest: Manifest = serde_json::from_str(raw)
            .map_err(|error| err(format!("invalid {MANIFEST_FILE}: {error}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        let mut problems: Vec<String> = Vec::new();
        if self.schema_version != 1 {
            problems.push("schemaVersion must be 1".into());
        }
        if !is_package_name(&self.name) {
            problems.push("name must be lowercase kebab-case, at most 64 characters".into());
        }
        if Version::parse(&self.version)
            .map(|version| version.to_string())
            .ok()
            .as_deref()
            != Some(&self.version)
        {
            problems.push("version must be an exact semantic version such as 1.2.3".into());
        }
        if !is_package_type(&self.kind) {
            problems.push("type must be a lowercase identifier such as \"project\"".into());
        }
        if self.description.chars().count() > 500 {
            problems.push("description must be at most 500 characters".into());
        }
        if self.dependencies.len() > MAX_DEPENDENCIES {
            problems.push(format!("at most {MAX_DEPENDENCIES} dependencies"));
        }
        for (field, value) in [
            ("provides", &self.provides),
            ("engines", &self.engines),
            ("provenance", &self.provenance),
        ] {
            if value.as_ref().is_some_and(|value| !value.is_object()) {
                problems.push(format!("{field} must be an object"));
            }
        }
        for (name, dependency_range) in &self.dependencies {
            if !is_package_name(name) {
                problems.push(format!(
                    "dependency name \"{name}\" is not a valid package name"
                ));
            } else if name == &self.name {
                problems.push("a package cannot depend on itself".into());
            } else if let Err(error) = range::parse(dependency_range) {
                problems.push(format!("dependency \"{name}\": {error}"));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(err(format!(
                "invalid {MANIFEST_FILE}: {}",
                problems.join("; ")
            )))
        }
    }

    pub fn semver(&self) -> Version {
        Version::parse(&self.version).expect("validated on parse")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str =
        r#"{"schemaVersion":1,"name":"team-handbook","version":"1.2.3","type":"team"}"#;

    fn problems(raw: &str) -> String {
        Manifest::parse(raw).unwrap_err().to_string()
    }

    #[test]
    fn parses_a_minimal_manifest() {
        let manifest = Manifest::parse(VALID).unwrap();
        assert_eq!(manifest.name, "team-handbook");
        assert_eq!(manifest.kind, "team");
        assert_eq!(manifest.description, "");
        assert!(manifest.dependencies.is_empty());
    }

    #[test]
    fn keeps_dependencies_and_ignores_unknown_fields() {
        let manifest = Manifest::parse(
            r#"{"schemaVersion":1,"name":"a","version":"2.0.0-rc.1","type":"project",
                "dependencies":{"base":"*","other":"^1.0.0"},"provides":{"skills":[]},"extra":true}"#,
        )
        .unwrap();
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.semver().to_string(), "2.0.0-rc.1");
    }

    #[test]
    fn package_names() {
        for good in ["a", "team-handbook", "project-42", "9lives"] {
            assert!(is_package_name(good), "{good}");
        }
        for bad in [
            "",
            "Team",
            "team_handbook",
            "-team",
            "team-",
            "team--x",
            "a/b",
            "__proto__",
            &"a".repeat(65),
        ] {
            assert!(!is_package_name(bad), "{bad}");
        }
    }

    #[test]
    fn rejects_invalid_fields() {
        assert!(
            problems(&VALID.replace("\"schemaVersion\":1", "\"schemaVersion\":2"))
                .contains("schemaVersion")
        );
        assert!(
            problems(&VALID.replace("team-handbook", "Team Handbook")).contains("name must be")
        );
        assert!(problems(&VALID.replace("1.2.3", "v1.2.3")).contains("version must be"));
        assert!(problems(&VALID.replace("1.2.3", "1.2")).contains("version must be"));
        assert!(problems(&VALID.replace("\"team\"", "\"Team\"")).contains("type must be"));
        assert!(problems("not json").contains("invalid manifest.json"));
        assert!(problems(r#"{"schemaVersion":1}"#).contains("invalid manifest.json"));
    }

    #[test]
    fn checks_the_same_limits_as_the_registry() {
        let many: Vec<String> = (0..201)
            .map(|index| format!("\"dep-{index}\":\"*\""))
            .collect();
        let too_many = VALID.replace(
            "\"type\":\"team\"",
            &format!("\"type\":\"team\",\"dependencies\":{{{}}}", many.join(",")),
        );
        assert!(problems(&too_many).contains("at most 200 dependencies"));
        for field in ["provides", "engines", "provenance"] {
            let bad = VALID.replace(
                "\"type\":\"team\"",
                &format!("\"type\":\"team\",\"{field}\":\"text\""),
            );
            assert!(
                problems(&bad).contains(&format!("{field} must be an object")),
                "{field}"
            );
        }
    }

    #[test]
    fn rejects_invalid_dependencies() {
        let with = |dependencies: &str| {
            VALID.replace(
                "\"type\":\"team\"",
                &format!("\"type\":\"team\",\"dependencies\":{dependencies}"),
            )
        };
        assert!(problems(&with(r#"{"Bad Name":"1.0.0"}"#)).contains("not a valid package name"));
        assert!(problems(&with(r#"{"team-handbook":"*"}"#)).contains("cannot depend on itself"));
        assert!(problems(&with(r#"{"other":"^1 || ^2"}"#)).contains("does not support"));
    }
}
