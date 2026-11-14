//! Client for one registry: reads the index from Firestore, downloads artifacts from
//! Storage and calls the write API. All over plain HTTPS, with the caller's own token, so
//! the registry's security rules decide what this client can see.

use std::collections::BTreeMap;

use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder};
use semver::Version;
use serde_json::{Map, Value, json};

use crate::descriptor::Descriptor;
use crate::error::{Result, err};

/// Fields of a version document the CLI reads.
const VERSION_FIELDS: [&str; 12] = [
    "name",
    "version",
    "type",
    "description",
    "dependencies",
    "integrity",
    "artifact",
    "archivePrefix",
    "size",
    "yanked",
    "publishedAt",
    "publishedBy",
];

/// A CI workflow that may publish a package. Only `repository` is required; every other
/// field that is set must match the publishing job.
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TrustedPublisher {
    pub provider: String,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

/// A package as listed in the registry index.
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub kind: String,
    pub description: String,
    pub visibility: String,
    pub latest: Option<String>,
    pub dist_tags: BTreeMap<String, String>,
    pub version_count: u64,
}

/// Who may read, maintain and publish a package. Only its maintainers can see this.
#[derive(Debug, Clone, Default)]
pub struct AccessInfo {
    pub visibility: String,
    pub readers: Vec<String>,
    pub reader_domains: Vec<String>,
    pub maintainers: Vec<String>,
    pub trusted_publishers: Vec<TrustedPublisher>,
}

/// One published version of a package.
#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub name: String,
    pub version: Version,
    pub kind: String,
    pub description: String,
    pub dependencies: BTreeMap<String, String>,
    /// `sha256-<hex>` of the archive.
    pub integrity: String,
    /// Object name of the archive in the registry's bucket.
    pub artifact: String,
    /// Top-level directory inside the archive that is stripped on install ("" for none).
    pub archive_prefix: String,
    pub size: u64,
    pub yanked: bool,
    pub published_at: Option<String>,
    pub published_by: Option<String>,
}

/// Converts a Firestore REST value (`{"stringValue": "x"}`) to plain JSON.
fn plain(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return Value::Null;
    };
    if let Some(text) = object.get("stringValue") {
        return text.clone();
    }
    if let Some(flag) = object.get("booleanValue") {
        return flag.clone();
    }
    if let Some(integer) = object.get("integerValue") {
        return integer
            .as_str()
            .and_then(|text| text.parse::<i64>().ok())
            .map(Value::from)
            .unwrap_or_else(|| integer.clone());
    }
    if let Some(double) = object.get("doubleValue") {
        return double.clone();
    }
    if let Some(timestamp) = object.get("timestampValue") {
        return timestamp.clone();
    }
    if let Some(map) = object.get("mapValue") {
        return fields(map);
    }
    if let Some(array) = object.get("arrayValue") {
        return Value::Array(
            array
                .get("values")
                .and_then(Value::as_array)
                .map(|values| values.iter().map(plain).collect())
                .unwrap_or_default(),
        );
    }
    Value::Null
}

/// Converts the `fields` of a Firestore document or map value to a plain JSON object.
fn fields(document: &Value) -> Value {
    let mut out = Map::new();
    if let Some(entries) = document.get("fields").and_then(Value::as_object) {
        for (key, value) in entries {
            out.insert(key.clone(), plain(value));
        }
    }
    Value::Object(out)
}

fn text(data: &Value, key: &str) -> String {
    data.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn strings(data: &Value, key: &str) -> Vec<String> {
    data.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn string_map(data: &Value, key: &str) -> BTreeMap<String, String> {
    data.get(key)
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn package_from(data: &Value) -> PackageInfo {
    let dist_tags = string_map(data, "distTags");
    PackageInfo {
        name: text(data, "name"),
        kind: text(data, "type"),
        description: text(data, "description"),
        visibility: text(data, "visibility"),
        latest: dist_tags.get("latest").cloned(),
        dist_tags,
        version_count: data
            .get("versionCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn access_from(data: &Value) -> AccessInfo {
    AccessInfo {
        visibility: text(data, "visibility"),
        readers: strings(data, "readers"),
        reader_domains: strings(data, "readerDomains"),
        maintainers: strings(data, "maintainers"),
        trusted_publishers: data
            .get("trustedPublishers")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| serde_json::from_value(item.clone()).ok())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn version_from(data: &Value) -> Option<VersionInfo> {
    let published_by = data.get("publishedBy").and_then(|by| {
        by.get("email")
            .or_else(|| by.get("repository"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    Some(VersionInfo {
        name: text(data, "name"),
        version: Version::parse(data.get("version")?.as_str()?).ok()?,
        kind: text(data, "type"),
        description: text(data, "description"),
        dependencies: string_map(data, "dependencies"),
        integrity: text(data, "integrity"),
        artifact: text(data, "artifact"),
        archive_prefix: text(data, "archivePrefix"),
        size: data.get("size").and_then(Value::as_u64).unwrap_or(0),
        yanked: data.get("yanked").and_then(Value::as_bool).unwrap_or(false),
        published_at: data
            .get("publishedAt")
            .and_then(Value::as_str)
            .map(str::to_string),
        published_by,
    })
}

/// Percent-encodes one path segment (everything except unreserved characters).
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The message of an error response from the write API or a Google API.
fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)?
                .to_string();
            let details = value
                .pointer("/error/details")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .filter(|details| !details.is_empty());
            Some(match details {
                Some(details) if !message.contains(&details) => format!("{message} ({details})"),
                _ => message,
            })
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

pub struct Registry {
    http: Client,
    descriptor: Descriptor,
    /// Bearer token of the caller; `None` means anonymous (public packages only).
    token: Option<String>,
    /// Verified email in the token, used to build the listing queries.
    email: Option<String>,
}

impl Registry {
    pub fn new(http: Client, descriptor: Descriptor, token: Option<String>) -> Registry {
        let email = token.as_deref().and_then(crate::auth::email_of);
        Registry {
            http,
            descriptor,
            token,
            email,
        }
    }

    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    pub fn is_signed_in(&self) -> bool {
        self.token.is_some()
    }

    fn documents(&self) -> String {
        format!(
            "{}/v1/projects/{}/databases/(default)/documents",
            self.descriptor.firestore(),
            self.descriptor.firebase.project_id
        )
    }

    fn authorized(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    /// The package, or `None` when it does not exist or the caller may not see it. The
    /// registry does not tell those two cases apart, on purpose.
    pub fn package(&self, name: &str) -> Result<Option<PackageInfo>> {
        let url = format!("{}/packages/{}", self.documents(), encode_segment(name));
        let response = self.authorized(self.http.get(url)).send()?;
        match response.status() {
            StatusCode::OK => Ok(Some(package_from(&fields(&response.json::<Value>()?)))),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => Ok(None),
            status => Err(err(format!(
                "registry returned {status} for package \"{name}\": {}",
                error_message(&response.text()?)
            ))),
        }
    }

    /// Every published version of a package, or `None` when the caller may not see it.
    pub fn versions(&self, name: &str) -> Result<Option<Vec<VersionInfo>>> {
        let base = format!(
            "{}/packages/{}/versions",
            self.documents(),
            encode_segment(name)
        );
        let mut versions = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut request = self
                .authorized(self.http.get(&base))
                .query(&[("pageSize", "300")])
                // Only what resolving and listing need; READMEs and file lists stay behind.
                .query(&VERSION_FIELDS.map(|field| ("mask.fieldPaths", field)));
            if let Some(token) = &page_token {
                request = request.query(&[("pageToken", token.as_str())]);
            }
            let response = request.send()?;
            match response.status() {
                StatusCode::OK => {}
                StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => return Ok(None),
                status => {
                    return Err(err(format!(
                        "registry returned {status} for the versions of \"{name}\": {}",
                        error_message(&response.text()?)
                    )));
                }
            }
            let body: Value = response.json()?;
            if let Some(documents) = body.get("documents").and_then(Value::as_array) {
                versions.extend(
                    documents
                        .iter()
                        .filter_map(|document| version_from(&fields(document))),
                );
            }
            page_token = body
                .get("nextPageToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }
        versions.sort_by(|a, b| b.version.cmp(&a.version));
        Ok(Some(versions))
    }

    /// Runs a structured query and returns the plain data of every matching document.
    fn run_query(&self, collection: &str, filter: Value, what: &str) -> Result<Vec<Value>> {
        let url = format!("{}:runQuery", self.documents());
        let body = json!({ "structuredQuery": { "from": [{ "collectionId": collection }], "where": filter } });
        let response = self.authorized(self.http.post(url)).json(&body).send()?;
        let status = response.status();
        if !status.is_success() {
            return Err(err(format!(
                "registry returned {status} while {what}: {}",
                error_message(&response.text()?)
            )));
        }
        let rows: Value = response.json()?;
        Ok(rows
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.get("document"))
                    .map(fields)
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Names of the restricted packages shared with the caller: with their address, as a
    /// maintainer, or with their email domain. The registry only shows a caller the
    /// grants that name them.
    fn granted_packages(&self, email: &str) -> Result<Vec<String>> {
        let equals = |field: &str, value: &str| json!({ "fieldFilter": { "field": { "fieldPath": field }, "op": "EQUAL", "value": { "stringValue": value } } });
        let all =
            |filters: Vec<Value>| json!({ "compositeFilter": { "op": "AND", "filters": filters } });
        let kinds = json!({ "fieldFilter": {
            "field": { "fieldPath": "kind" },
            "op": "IN",
            "value": { "arrayValue": { "values": [{ "stringValue": "reader" }, { "stringValue": "maintainer" }] } },
        }});

        let mut grants = self.run_query(
            "grants",
            all(vec![kinds, equals("principal", email)]),
            "looking up what is shared with you",
        )?;
        if let Some((_, domain)) = email.split_once('@') {
            grants.extend(self.run_query(
                "grants",
                all(vec![equals("kind", "domain"), equals("principal", domain)]),
                "looking up what is shared with your domain",
            )?);
        }
        let mut names: Vec<String> = grants
            .iter()
            .map(|grant| text(grant, "package"))
            .filter(|name| !name.is_empty())
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// Every package the caller may read: the public ones, plus what is shared with them.
    pub fn packages(&self) -> Result<Vec<PackageInfo>> {
        let public = json!({ "fieldFilter": {
            "field": { "fieldPath": "visibility" },
            "op": "EQUAL",
            "value": { "stringValue": "public" },
        }});
        let mut found: BTreeMap<String, PackageInfo> = self
            .run_query("packages", public, "listing packages")?
            .iter()
            .map(|data| {
                let package = package_from(data);
                (package.name.clone(), package)
            })
            .collect();
        if let Some(email) = &self.email {
            for name in self.granted_packages(email)? {
                if found.contains_key(&name) {
                    continue;
                }
                // A grant can outlive a package for a moment; skip what is not there.
                if let Some(package) = self.package(&name)? {
                    found.insert(name, package);
                }
            }
        }
        Ok(found.into_values().collect())
    }

    /// Who has access to a package, or `None` when the caller is not one of its
    /// maintainers (or the package does not exist).
    pub fn access(&self, name: &str) -> Result<Option<AccessInfo>> {
        let url = format!(
            "{}/packages/{}/private/access",
            self.documents(),
            encode_segment(name)
        );
        let response = self.authorized(self.http.get(url)).send()?;
        match response.status() {
            StatusCode::OK => Ok(Some(access_from(&fields(&response.json::<Value>()?)))),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => Ok(None),
            status => Err(err(format!(
                "registry returned {status} for the access of \"{name}\": {}",
                error_message(&response.text()?)
            ))),
        }
    }

    /// Downloads an artifact by its object name.
    pub fn download(&self, artifact: &str) -> Result<Vec<u8>> {
        let url = format!(
            "{}/v0/b/{}/o/{}",
            self.descriptor.storage(),
            self.descriptor.firebase.storage_bucket,
            encode_segment(artifact)
        );
        let mut request = self.http.get(url).query(&[("alt", "media")]);
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Firebase {token}"));
        }
        let response = request.send()?;
        let status = response.status();
        if !status.is_success() {
            return Err(err(format!(
                "cannot download {artifact}: registry returned {status}"
            )));
        }
        Ok(response.bytes()?.to_vec())
    }

    fn api(&self, path: &str) -> Result<RequestBuilder> {
        let token = self
            .token
            .as_ref()
            .ok_or_else(|| err("you are not signed in; run `ctx login`"))?;
        Ok(self
            .http
            .post(format!("{}{path}", self.descriptor.api()))
            .bearer_auth(token))
    }

    fn api_result(response: reqwest::blocking::Response) -> Result<Value> {
        let status = response.status();
        let body = response.text()?;
        if status.is_success() {
            return Ok(serde_json::from_str(&body).unwrap_or(Value::Null));
        }
        Err(err(error_message(&body)))
    }

    /// Publishes an archive. Returns the registry's answer (name, version, integrity, distTags).
    pub fn publish(&self, archive: Vec<u8>, tag: Option<&str>) -> Result<Value> {
        let mut request = self
            .api("/publish")?
            .header("Content-Type", "application/gzip")
            .body(archive);
        if let Some(tag) = tag {
            request = request.query(&[("tag", tag)]);
        }
        Self::api_result(request.send()?)
    }

    /// Claims a package name and sets its access lists.
    pub fn create_package(&self, package: &Value) -> Result<Value> {
        Self::api_result(self.api("/packages")?.json(package).send()?)
    }

    pub fn yank(&self, name: &str, version: &str, yanked: bool) -> Result<Value> {
        let path = format!(
            "/packages/{}/versions/{}/yank",
            encode_segment(name),
            encode_segment(version)
        );
        Self::api_result(self.api(&path)?.json(&json!({ "yanked": yanked })).send()?)
    }

    /// Changes who has access to a package. `change` holds any of visibility, readers,
    /// readerDomains, maintainers and trustedPublishers; what is left out stays as it is.
    /// Only maintainers are allowed to.
    pub fn set_access(&self, name: &str, change: &Value) -> Result<()> {
        let token = self
            .token
            .as_ref()
            .ok_or_else(|| err("you are not signed in; run `ctx login`"))?;
        let url = format!(
            "{}/packages/{}/access",
            self.descriptor.api(),
            encode_segment(name)
        );
        Self::api_result(
            self.http
                .patch(url)
                .bearer_auth(token)
                .json(change)
                .send()?,
        )
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_firestore_document() {
        let document = json!({
            "name": "projects/p/databases/(default)/documents/packages/team-handbook",
            "fields": {
                "name": { "stringValue": "team-handbook" },
                "type": { "stringValue": "team" },
                "visibility": { "stringValue": "restricted" },
                "versionCount": { "integerValue": "3" },
                "distTags": { "mapValue": { "fields": { "latest": { "stringValue": "1.2.0" } } } },
                "latestVersion": { "nullValue": null },
            }
        });
        let package = package_from(&fields(&document));
        assert_eq!(package.name, "team-handbook");
        assert_eq!(package.kind, "team");
        assert_eq!(package.version_count, 3);
        assert_eq!(package.latest.as_deref(), Some("1.2.0"));
    }

    #[test]
    fn decodes_a_version_and_skips_malformed_ones() {
        let document = json!({ "fields": {
            "name": { "stringValue": "a" },
            "version": { "stringValue": "1.0.0" },
            "integrity": { "stringValue": "sha256-abc" },
            "artifact": { "stringValue": "packages/a/1.0.0/abc.tgz" },
            "archivePrefix": { "stringValue": "package/" },
            "size": { "integerValue": "120" },
            "yanked": { "booleanValue": true },
            "dependencies": { "mapValue": { "fields": { "base": { "stringValue": "^1.0.0" } } } },
            "publishedBy": { "mapValue": { "fields": { "kind": { "stringValue": "github" }, "repository": { "stringValue": "acme/a" } } } },
        }});
        let version = version_from(&fields(&document)).unwrap();
        assert_eq!(version.version, Version::new(1, 0, 0));
        assert!(version.yanked);
        assert_eq!(version.size, 120);
        assert_eq!(version.archive_prefix, "package/");
        assert_eq!(
            version.dependencies.get("base").map(String::as_str),
            Some("^1.0.0")
        );
        assert_eq!(version.published_by.as_deref(), Some("acme/a"));

        assert!(
            version_from(&fields(
                &json!({ "fields": { "version": { "stringValue": "nope" } } })
            ))
            .is_none()
        );
    }

    #[test]
    fn decodes_an_access_document() {
        let document = json!({ "fields": {
            "visibility": { "stringValue": "restricted" },
            "readers": { "arrayValue": { "values": [{ "stringValue": "a@example.dev" }] } },
            "readerDomains": { "arrayValue": {} },
            "maintainers": { "arrayValue": { "values": [{ "stringValue": "owner@example.dev" }] } },
            "trustedPublishers": { "arrayValue": { "values": [{ "mapValue": { "fields": {
                "provider": { "stringValue": "github" },
                "repository": { "stringValue": "acme/handbook" },
                "ref": { "stringValue": "refs/heads/main" },
            }}}]}},
        }});
        let access = access_from(&fields(&document));
        assert_eq!(access.visibility, "restricted");
        assert_eq!(access.readers, vec!["a@example.dev"]);
        assert!(access.reader_domains.is_empty());
        assert_eq!(access.maintainers, vec!["owner@example.dev"]);
        assert_eq!(access.trusted_publishers.len(), 1);
        assert_eq!(access.trusted_publishers[0].repository, "acme/handbook");
        assert_eq!(
            access.trusted_publishers[0].git_ref.as_deref(),
            Some("refs/heads/main")
        );
        assert_eq!(access.trusted_publishers[0].workflow, None);
    }

    #[test]
    fn encodes_path_segments() {
        assert_eq!(
            encode_segment("packages/a/1.0.0+build/x.tgz"),
            "packages%2Fa%2F1.0.0%2Bbuild%2Fx.tgz"
        );
        assert_eq!(encode_segment("team-handbook_1.~"), "team-handbook_1.~");
    }

    #[test]
    fn reads_api_error_messages() {
        assert_eq!(
            error_message(r#"{"error":{"code":"forbidden","message":"Not allowed"}}"#),
            "Not allowed"
        );
        assert_eq!(
            error_message(
                r#"{"error":{"message":"Invalid manifest","details":["name must be x","version must be y"]}}"#
            ),
            "Invalid manifest (name must be x; version must be y)"
        );
        assert_eq!(error_message("<html>oops</html>"), "<html>oops</html>");
    }
}
