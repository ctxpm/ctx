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
    pub readers: Vec<String>,
    pub reader_domains: Vec<String>,
    pub maintainers: Vec<String>,
    pub trusted_publishers: Vec<TrustedPublisher>,
    pub version_count: u64,
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
        version_count: data
            .get("versionCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
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

    fn query(&self, field: &str, operator: &str, value: &str) -> Result<Vec<PackageInfo>> {
        let url = format!("{}:runQuery", self.documents());
        let body = json!({
            "structuredQuery": {
                "from": [{ "collectionId": "packages" }],
                "where": { "fieldFilter": {
                    "field": { "fieldPath": field },
                    "op": operator,
                    "value": { "stringValue": value },
                }},
            }
        });
        let response = self.authorized(self.http.post(url)).json(&body).send()?;
        let status = response.status();
        if !status.is_success() {
            return Err(err(format!(
                "registry returned {status} while listing packages: {}",
                error_message(&response.text()?)
            )));
        }
        let rows: Value = response.json()?;
        Ok(rows
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.get("document"))
                    .map(|document| package_from(&fields(document)))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Every package the caller may read. The registry only answers queries that are
    /// limited to what the caller is entitled to, so this asks once per kind of
    /// entitlement and merges the answers.
    pub fn packages(&self) -> Result<Vec<PackageInfo>> {
        let mut found: BTreeMap<String, PackageInfo> = BTreeMap::new();
        let mut add = |packages: Vec<PackageInfo>| {
            for package in packages {
                found.entry(package.name.clone()).or_insert(package);
            }
        };
        add(self.query("visibility", "EQUAL", "public")?);
        if let Some(email) = &self.email {
            add(self.query("readers", "ARRAY_CONTAINS", email)?);
            add(self.query("maintainers", "ARRAY_CONTAINS", email)?);
            if let Some((_, domain)) = email.split_once('@') {
                add(self.query("readerDomains", "ARRAY_CONTAINS", domain)?);
            }
        }
        Ok(found.into_values().collect())
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

    /// Replaces the access lists of a package. Only maintainers are allowed to.
    pub fn set_access(&self, name: &str, access: &BTreeMap<&str, Value>) -> Result<()> {
        let token = self
            .token
            .as_ref()
            .ok_or_else(|| err("you are not signed in; run `ctx login`"))?;
        let url = format!("{}/packages/{}", self.documents(), encode_segment(name));
        let mut request = self.http.patch(url).bearer_auth(token);
        let mut document = Map::new();
        for (field, value) in access {
            request = request.query(&[("updateMask.fieldPaths", *field)]);
            document.insert((*field).to_string(), firestore_value(value));
        }
        let response = request
            .query(&[("currentDocument.exists", "true")])
            .json(&json!({ "fields": document }))
            .send()?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        if status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND {
            return Err(err(format!(
                "you are not a maintainer of \"{name}\", or it does not exist"
            )));
        }
        Err(err(format!(
            "registry returned {status}: {}",
            error_message(&response.text()?)
        )))
    }
}

/// Converts plain JSON to a Firestore REST value.
fn firestore_value(value: &Value) -> Value {
    match value {
        Value::Null => json!({ "nullValue": null }),
        Value::String(text) => json!({ "stringValue": text }),
        Value::Bool(flag) => json!({ "booleanValue": flag }),
        Value::Number(number) if number.is_i64() || number.is_u64() => {
            json!({ "integerValue": number.to_string() })
        }
        Value::Number(number) => json!({ "doubleValue": number }),
        Value::Array(items) => {
            json!({ "arrayValue": { "values": items.iter().map(firestore_value).collect::<Vec<_>>() } })
        }
        Value::Object(entries) => {
            let fields: Map<String, Value> = entries
                .iter()
                .map(|(key, value)| (key.clone(), firestore_value(value)))
                .collect();
            json!({ "mapValue": { "fields": fields } })
        }
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
                "readers": { "arrayValue": { "values": [{ "stringValue": "a@example.dev" }] } },
                "readerDomains": { "arrayValue": {} },
                "distTags": { "mapValue": { "fields": { "latest": { "stringValue": "1.2.0" } } } },
                "latestVersion": { "nullValue": null },
            }
        });
        let package = package_from(&fields(&document));
        assert_eq!(package.name, "team-handbook");
        assert_eq!(package.kind, "team");
        assert_eq!(package.version_count, 3);
        assert_eq!(package.readers, vec!["a@example.dev"]);
        assert!(package.reader_domains.is_empty());
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
    fn firestore_values_round_trip_through_plain_json() {
        let original = json!({
            "trustedPublishers": [
                { "provider": "github", "repository": "acme/handbook", "ref": "refs/heads/main" }
            ],
            "readers": ["a@example.dev"],
            "count": 3,
            "open": false,
        });
        let document = json!({ "fields": firestore_value(&original)["mapValue"]["fields"] });
        assert_eq!(fields(&document), original);

        let package = package_from(&fields(&document));
        assert_eq!(package.trusted_publishers.len(), 1);
        assert_eq!(package.trusted_publishers[0].repository, "acme/handbook");
        assert_eq!(
            package.trusted_publishers[0].git_ref.as_deref(),
            Some("refs/heads/main")
        );
        assert_eq!(package.trusted_publishers[0].workflow, None);
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
