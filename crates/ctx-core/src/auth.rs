//! Signing in: Google sign-in through the browser, exchanged for a Firebase session that
//! is refreshed when it expires.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::Session;
use crate::descriptor::Descriptor;
use crate::error::{Result, err};

const GOOGLE_AUTHORIZE: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN: &str = "https://oauth2.googleapis.com/token";
/// How long `ctx login` waits for the browser to come back.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn random_token(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer)
        .map_err(|error| err(format!("cannot get random bytes: {error}")))?;
    Ok(URL_SAFE_NO_PAD.encode(buffer))
}

/// Reads the claims of a JWT without verifying it. Only used to show who is signed in and
/// to build queries; the registry verifies every token itself.
pub fn claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?).ok()
}

/// The verified email address in a token, lower-cased.
pub fn email_of(token: &str) -> Option<String> {
    let claims = claims(token)?;
    if claims.get("email_verified").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    claims
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_lowercase)
}

/// Turns an error response of a Google API into one line.
fn api_error(what: &str, status: reqwest::StatusCode, body: &str) -> crate::Error {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.pointer("/error_description"))
                .or_else(|| value.pointer("/error"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    err(format!("{what} failed ({status}): {detail}"))
}

fn session_from(
    body: &Value,
    id_field: &str,
    refresh_field: &str,
    expires_field: &str,
) -> Result<Session> {
    let text = |field: &str| body.get(field).and_then(Value::as_str).map(str::to_string);
    let id_token = text(id_field).ok_or_else(|| err("sign-in response has no ID token"))?;
    let refresh_token =
        text(refresh_field).ok_or_else(|| err("sign-in response has no refresh token"))?;
    let expires_in = body
        .get(expires_field)
        .and_then(|value| {
            value
                .as_str()
                .and_then(|text| text.parse::<u64>().ok())
                .or_else(|| value.as_u64())
        })
        .unwrap_or(3600);
    let email = email_of(&id_token).ok_or_else(|| {
        err("your Google account has no verified email address, which the registry requires")
    })?;
    Ok(Session {
        email,
        refresh_token,
        id_token,
        expires_at: now() + expires_in,
    })
}

/// Exchanges a Google ID token for a Firebase session.
pub fn sign_in_with_google(
    http: &Client,
    descriptor: &Descriptor,
    google_id_token: &str,
) -> Result<Session> {
    let url = format!(
        "{}/v1/accounts:signInWithIdp",
        descriptor.identity_toolkit()
    );
    let response = http
        .post(url)
        .query(&[("key", descriptor.firebase.api_key.as_str())])
        .json(&json!({
            "postBody": format!("id_token={}&providerId=google.com", urlencode(google_id_token)),
            "requestUri": "http://localhost",
            "returnIdpCredential": true,
            "returnSecureToken": true,
        }))
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(api_error("sign-in", status, &body));
    }
    session_from(
        &serde_json::from_str(&body)?,
        "idToken",
        "refreshToken",
        "expiresIn",
    )
}

/// Gets a fresh ID token for an existing session.
pub fn refresh(http: &Client, descriptor: &Descriptor, session: &Session) -> Result<Session> {
    let url = format!("{}/v1/token", descriptor.secure_token());
    let response = http
        .post(url)
        .query(&[("key", descriptor.firebase.api_key.as_str())])
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", session.refresh_token.as_str()),
        ])
        .send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(err(format!(
            "your session can no longer be refreshed ({}); run `ctx login` again",
            api_error("refresh", status, &body)
        )));
    }
    session_from(
        &serde_json::from_str(&body)?,
        "id_token",
        "refresh_token",
        "expires_in",
    )
}

/// True when the session's ID token is still valid for at least a minute.
pub fn is_fresh(session: &Session) -> bool {
    session.expires_at > now() + 60
}

fn urlencode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Signs in to the Auth emulator as the given address. Real Google sign-in rejects this,
/// so it only works against a registry whose descriptor points at an emulator.
pub fn sign_in_emulated(http: &Client, descriptor: &Descriptor, email: &str) -> Result<Session> {
    if !descriptor.uses_emulated_auth() {
        return Err(err(
            "--dev-email only works with a registry that runs on the Firebase emulators",
        ));
    }
    let fake = json!({ "sub": format!("dev-{email}"), "email": email, "email_verified": true })
        .to_string();
    sign_in_with_google(http, descriptor, &fake)
}

fn open_in_browser(url: &str) -> bool {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg(url);
        command
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Waits for the browser to be redirected back to the local listener and returns the query
/// parameters of that request.
fn wait_for_redirect(listener: TcpListener) -> Result<Vec<(String, String)>> {
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + LOGIN_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                // Browsers open spare connections that never send anything. A connection
                // that fails or stays silent is dropped; only the redirect ends the wait.
                let mut request_line = String::new();
                let readable = stream.set_nonblocking(false).is_ok()
                    && stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .is_ok()
                    && BufReader::new(&stream).read_line(&mut request_line).is_ok();
                if !readable || request_line.trim().is_empty() {
                    continue;
                }
                let target = request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                let query = target.split_once('?').map(|(_, query)| query).unwrap_or("");
                let params: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect();
                // Browsers also ask for /favicon.ico; only the redirect carries parameters.
                let is_redirect = params
                    .iter()
                    .any(|(key, _)| key == "code" || key == "error");
                let page = if is_redirect {
                    "<!doctype html><meta charset=utf-8><title>ctx</title><p style=\"font:16px system-ui;margin:3em\">Signed in. You can close this tab and return to the terminal.</p>"
                } else {
                    ""
                };
                let status = if is_redirect {
                    "200 OK"
                } else {
                    "404 Not Found"
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                );
                if is_redirect {
                    return Ok(params);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    return Err(err("timed out waiting for the browser sign-in"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

/// Signs in with Google in the browser (authorization code flow with PKCE and a loopback
/// redirect) and exchanges the result for a Firebase session.
pub fn sign_in_with_browser(
    http: &Client,
    descriptor: &Descriptor,
    announce: impl Fn(&str),
) -> Result<Session> {
    let oauth = descriptor.oauth.as_ref().ok_or_else(|| {
        err("this registry does not publish an OAuth client, so `ctx login` cannot sign you in")
    })?;

    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let verifier = random_token(48)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_token(16)?;

    let mut authorize =
        url::Url::parse(GOOGLE_AUTHORIZE).map_err(|error| err(error.to_string()))?;
    authorize
        .query_pairs_mut()
        .append_pair("client_id", &oauth.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", "openid email")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("prompt", "select_account");

    if open_in_browser(authorize.as_str()) {
        announce("Your browser opened to sign in with Google. If it did not, open this address:");
    } else {
        announce("Open this address in a browser to sign in with Google:");
    }
    announce(authorize.as_str());

    let params = wait_for_redirect(listener)?;
    let param = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    if let Some(error) = param("error") {
        return Err(err(format!("Google sign-in was not completed: {error}")));
    }
    if param("state") != Some(state.as_str()) {
        return Err(err(
            "sign-in response did not match this request; try again",
        ));
    }
    let code = param("code").ok_or_else(|| err("sign-in response has no authorization code"))?;

    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", oauth.client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_verifier", verifier.as_str()),
    ];
    if let Some(secret) = oauth.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    let response = http.post(GOOGLE_TOKEN).form(&form).send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(api_error("Google sign-in", status, &body));
    }
    let tokens: Value = serde_json::from_str(&body)?;
    let google_id_token = tokens
        .get("id_token")
        .and_then(Value::as_str)
        .ok_or_else(|| err("Google did not return an ID token"))?;
    sign_in_with_google(http, descriptor, google_id_token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    #[test]
    fn the_login_listener_ignores_idle_and_unrelated_connections() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiting = std::thread::spawn(move || wait_for_redirect(listener));

        // A spare connection that is closed without a request, as browsers make.
        drop(TcpStream::connect(("127.0.0.1", port)).unwrap());
        // One that stays open and silent for longer than the read timeout.
        let idle = TcpStream::connect(("127.0.0.1", port)).unwrap();
        // A request that is not the redirect.
        let mut favicon = TcpStream::connect(("127.0.0.1", port)).unwrap();
        favicon
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        // The redirect itself.
        let mut redirect = TcpStream::connect(("127.0.0.1", port)).unwrap();
        redirect
            .write_all(b"GET /?state=abc&code=4%2Fxyz HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();

        let params = waiting.join().unwrap().unwrap();
        assert!(params.contains(&("code".to_string(), "4/xyz".to_string())));
        assert!(params.contains(&("state".to_string(), "abc".to_string())));
        drop(idle);
    }

    fn token(claims: Value) -> String {
        format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(claims.to_string())
        )
    }

    #[test]
    fn reads_the_verified_email_in_lower_case() {
        assert_eq!(
            email_of(&token(
                json!({ "email": "Reader@Example.dev", "email_verified": true })
            )),
            Some("reader@example.dev".to_string())
        );
    }

    #[test]
    fn ignores_unverified_or_malformed_tokens() {
        assert_eq!(
            email_of(&token(
                json!({ "email": "a@b.dev", "email_verified": false })
            )),
            None
        );
        assert_eq!(email_of(&token(json!({ "email": "a@b.dev" }))), None);
        assert_eq!(email_of("not-a-token"), None);
        assert_eq!(email_of("a.%%%.c"), None);
    }

    #[test]
    fn freshness_needs_a_minute_of_margin() {
        let session = |expires_at| Session {
            email: "a@b.dev".into(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            expires_at,
        };
        assert!(is_fresh(&session(now() + 600)));
        assert!(!is_fresh(&session(now() + 30)));
        assert!(!is_fresh(&session(0)));
    }
}
