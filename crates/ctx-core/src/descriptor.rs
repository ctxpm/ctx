//! The registry descriptor: the public document that tells a client how to reach a
//! registry. See `spec/registry.schema.json`.

use serde::{Deserialize, Serialize};

use crate::error::{Result, err};

pub const WELL_KNOWN_PATH: &str = "/.well-known/ctx-registry.json";

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub firebase: Firebase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuth>,
    /// Base URL of the write API, including the version segment, without a trailing slash.
    pub api: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc_audience: Option<String>,
    /// Overrides of the Google service URLs; only set for emulators and tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<Endpoints>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Firebase {
    pub project_id: String,
    pub api_key: String,
    pub storage_bucket: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuth {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_toolkit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secure_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firestore: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
}

impl Descriptor {
    pub fn parse(raw: &str) -> Result<Descriptor> {
        let descriptor: Descriptor = serde_json::from_str(raw)
            .map_err(|error| err(format!("invalid registry descriptor: {error}")))?;
        if descriptor.schema_version != 1 {
            return Err(err(format!(
                "registry descriptor has schemaVersion {}; this version of ctx understands 1",
                descriptor.schema_version
            )));
        }
        if descriptor.firebase.project_id.is_empty() || descriptor.api.is_empty() {
            return Err(err(
                "registry descriptor is missing firebase.projectId or api",
            ));
        }
        Ok(descriptor)
    }

    fn endpoint(&self, pick: impl Fn(&Endpoints) -> &Option<String>, default: &str) -> String {
        self.endpoints
            .as_ref()
            .and_then(|endpoints| pick(endpoints).clone())
            .unwrap_or_else(|| default.to_string())
            .trim_end_matches('/')
            .to_string()
    }

    pub fn identity_toolkit(&self) -> String {
        self.endpoint(
            |endpoints| &endpoints.identity_toolkit,
            "https://identitytoolkit.googleapis.com",
        )
    }

    pub fn secure_token(&self) -> String {
        self.endpoint(
            |endpoints| &endpoints.secure_token,
            "https://securetoken.googleapis.com",
        )
    }

    pub fn firestore(&self) -> String {
        self.endpoint(
            |endpoints| &endpoints.firestore,
            "https://firestore.googleapis.com",
        )
    }

    pub fn storage(&self) -> String {
        self.endpoint(
            |endpoints| &endpoints.storage,
            "https://firebasestorage.googleapis.com",
        )
    }

    pub fn api(&self) -> &str {
        self.api.trim_end_matches('/')
    }

    /// True when sign-in goes to something other than Google, i.e. an emulator.
    pub fn uses_emulated_auth(&self) -> bool {
        self.endpoints
            .as_ref()
            .is_some_and(|endpoints| endpoints.identity_toolkit.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{"schemaVersion":1,"firebase":{"projectId":"p","apiKey":"k","storageBucket":"b"},"api":"https://r.example.dev/v1/"}"#;

    #[test]
    fn defaults_to_google_endpoints() {
        let descriptor = Descriptor::parse(MINIMAL).unwrap();
        assert_eq!(descriptor.firestore(), "https://firestore.googleapis.com");
        assert_eq!(
            descriptor.storage(),
            "https://firebasestorage.googleapis.com"
        );
        assert_eq!(descriptor.api(), "https://r.example.dev/v1");
        assert!(!descriptor.uses_emulated_auth());
    }

    #[test]
    fn honours_endpoint_overrides() {
        let raw = MINIMAL.replace(
            "\"api\"",
            "\"endpoints\":{\"firestore\":\"http://127.0.0.1:8085/\",\"identityToolkit\":\"http://127.0.0.1:9099/x\"},\"api\"",
        );
        let descriptor = Descriptor::parse(&raw).unwrap();
        assert_eq!(descriptor.firestore(), "http://127.0.0.1:8085");
        assert_eq!(
            descriptor.storage(),
            "https://firebasestorage.googleapis.com"
        );
        assert!(descriptor.uses_emulated_auth());
    }

    #[test]
    fn rejects_unknown_schema_versions_and_garbage() {
        assert!(
            Descriptor::parse(&MINIMAL.replace("\"schemaVersion\":1", "\"schemaVersion\":2"))
                .is_err()
        );
        assert!(Descriptor::parse("<html>").is_err());
    }
}
