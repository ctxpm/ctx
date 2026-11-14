//! End-to-end test of the `ctx` binary against a registry running on the Firebase
//! emulators. Run it with `scripts/e2e.sh`; without `CTX_E2E=1` it does nothing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use ctx_core::reqwest::blocking::Client;
use serde_json::{Value, json};

const FIRESTORE: &str = "http://127.0.0.1:8085/v1/projects/demo-ctx/databases/(default)/documents";

struct Output {
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Output {
    fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// One person: their own config directory, so their own registry list and session.
struct Person {
    config: tempfile::TempDir,
}

impl Person {
    fn new() -> Person {
        let person = Person {
            config: tempfile::tempdir().unwrap(),
        };
        let descriptor =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/emulator-registry.json");
        person.ok(
            Path::new("."),
            &["registry", "add", descriptor.to_str().unwrap()],
        );
        person
    }

    fn signed_in(email: &str) -> Person {
        let person = Person::new();
        let out = person.ok(Path::new("."), &["login", "--dev-email", email]);
        assert!(out.stdout.contains(&format!("as {email}")), "{}", out.all());
        person
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_ctx"))
            .args(args)
            .current_dir(cwd)
            .env("CTX_CONFIG_DIR", self.config.path())
            .env_remove("CTX_TOKEN")
            .output()
            .expect("cannot run ctx");
        Output {
            ok: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Runs a command that must succeed.
    fn ok(&self, cwd: &Path, args: &[&str]) -> Output {
        let out = self.run(cwd, args);
        assert!(out.ok, "`ctx {}` failed:\n{}", args.join(" "), out.all());
        out
    }

    /// Runs a command that must fail, and returns what it said.
    fn fails(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.run(cwd, args);
        assert!(
            !out.ok,
            "`ctx {}` should have failed:\n{}",
            args.join(" "),
            out.all()
        );
        out.stderr
    }
}

/// Writes a package directory and returns its path.
fn package(
    root: &Path,
    name: &str,
    version: &str,
    dependencies: &[(&str, &str)],
    body: &str,
) -> PathBuf {
    let dir = root.join(format!("{name}-{version}"));
    fs::create_dir_all(dir.join("docs")).unwrap();
    let dependencies: serde_json::Map<String, Value> = dependencies
        .iter()
        .map(|(name, range)| (name.to_string(), json!(range)))
        .collect();
    fs::write(
        dir.join("manifest.json"),
        json!({ "schemaVersion": 1, "name": name, "version": version, "type": "project", "description": format!("{name} for the e2e test"), "dependencies": dependencies })
            .to_string(),
    )
    .unwrap();
    fs::write(dir.join("README.md"), format!("# {name}\n")).unwrap();
    fs::write(dir.join("docs/body.md"), body).unwrap();
    dir
}

fn read(path: PathBuf) -> String {
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

#[test]
fn publish_install_update_and_access() {
    if std::env::var("CTX_E2E").as_deref() != Ok("1") {
        eprintln!("skipped: run scripts/e2e.sh to test against the Firebase emulators");
        return;
    }

    // Unique names, so the test can run again against the same emulators.
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        % 100_000_000;
    let base = format!("base-{id}");
    let app = format!("app-{id}");
    let secret = format!("secret-{id}");
    let bundle = format!("bundle-{id}");
    let open = format!("open-{id}");
    let owner_email = format!("owner-{id}@example.dev");
    let reader_email = format!("reader-{id}@example.dev");

    // The emulator accepts "Bearer owner" as an administrator; make the owner a creator.
    let seeded = Client::new()
        .patch(format!("{FIRESTORE}/registry/config"))
        .bearer_auth("owner")
        .json(&json!({ "fields": { "creators": { "arrayValue": { "values": [{ "stringValue": owner_email }] } } } }))
        .send()
        .expect("is the Firestore emulator running?");
    assert!(
        seeded.status().is_success(),
        "{}",
        seeded.text().unwrap_or_default()
    );

    let work = tempfile::tempdir().unwrap();
    let sources = work.path().join("sources");
    let owner = Person::signed_in(&owner_email);
    let reader = Person::signed_in(&reader_email);
    let stranger = Person::signed_in(&format!("stranger-{id}@elsewhere.dev"));
    let anonymous = Person::new();
    let cwd = work.path();

    assert!(owner.ok(cwd, &["whoami"]).stdout.contains(&owner_email));
    assert!(
        anonymous
            .ok(cwd, &["whoami"])
            .stdout
            .contains("Not signed in")
    );

    // --- Publishing -------------------------------------------------------------------
    let base_1_0 = package(&sources, &base, "1.0.0", &[], "base one\n");
    let app_1_0 = package(&sources, &app, "1.0.0", &[(&base, "^1.0.0")], "app one\n");
    assert!(
        owner
            .ok(cwd, &["publish", base_1_0.to_str().unwrap()])
            .stdout
            .contains(&format!("Published {base}@1.0.0"))
    );
    owner.ok(cwd, &["publish", app_1_0.to_str().unwrap()]);

    let again = owner.fails(cwd, &["publish", base_1_0.to_str().unwrap()]);
    assert!(again.contains("already published"), "{again}");

    let not_creator = reader.fails(
        cwd,
        &[
            "publish",
            package(&sources, &format!("mine-{id}"), "1.0.0", &[], "x")
                .to_str()
                .unwrap(),
        ],
    );
    assert!(
        not_creator.contains("not allowed to create packages"),
        "{not_creator}"
    );
    let not_maintainer = reader.fails(
        cwd,
        &[
            "publish",
            package(&sources, &base, "9.9.9", &[], "x")
                .to_str()
                .unwrap(),
        ],
    );
    assert!(
        not_maintainer.contains("not allowed to publish"),
        "{not_maintainer}"
    );
    let signed_out = anonymous.fails(cwd, &["publish", base_1_0.to_str().unwrap()]);
    assert!(signed_out.contains("not signed in"), "{signed_out}");

    // --- Access -----------------------------------------------------------------------
    let project = work.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let denied = reader.fails(&project, &["install", &app]);
    assert!(
        denied.contains("was not found in the registry, or you do not have access"),
        "{denied}"
    );
    assert!(
        !project.join("ctx.toml").exists() && !project.join(".ctx").exists(),
        "a failed install must leave nothing behind"
    );
    assert!(!reader.ok(cwd, &["search"]).stdout.contains(&app));

    owner.ok(cwd, &["access", "grant", &app, &reader_email]);
    owner.ok(cwd, &["access", "grant", &base, &reader_email]);
    assert!(
        owner
            .ok(cwd, &["access", "list", &app])
            .stdout
            .contains(&reader_email)
    );
    let not_yours = reader.fails(cwd, &["access", "grant", &app, "someone@example.dev"]);
    assert!(not_yours.contains("not a maintainer"), "{not_yours}");

    // --- Installing -------------------------------------------------------------------
    let installed = reader.ok(&project, &["install", &app]);
    assert!(
        installed.stdout.contains(&format!("+ {app} 1.0.0"))
            && installed.stdout.contains(&format!("+ {base} 1.0.0")),
        "{}",
        installed.all()
    );
    assert_eq!(
        read(project.join(format!(".ctx/packages/{app}/docs/body.md"))),
        "app one\n"
    );
    assert_eq!(
        read(project.join(format!(".ctx/packages/{base}/docs/body.md"))),
        "base one\n"
    );
    assert!(read(project.join("ctx.toml")).contains(&format!("{app} = \"^1.0.0\"")));
    assert!(
        !read(project.join("ctx.toml")).contains(&base),
        "dependencies are not written to ctx.toml"
    );
    let lock: Value = serde_json::from_str(&read(project.join("ctx.lock"))).unwrap();
    assert_eq!(lock["packages"][&app]["version"], "1.0.0");
    assert!(
        lock["packages"][&base]["integrity"]
            .as_str()
            .unwrap()
            .starts_with("sha256-")
    );

    let listed = reader.ok(&project, &["list"]).stdout;
    assert!(
        listed.contains(&format!("{app} 1.0.0")) && listed.contains("(dependency)"),
        "{listed}"
    );
    assert!(reader.ok(cwd, &["search", "app-"]).stdout.contains(&app));
    assert!(
        reader
            .ok(cwd, &["info", &app])
            .stdout
            .contains(&format!("needs {base}@^1.0.0"))
    );

    let no_access = stranger.fails(&work.path().join("project"), &["info", &app]);
    assert!(
        no_access.contains("was not found, or you do not have access"),
        "{no_access}"
    );
    assert!(!stranger.ok(cwd, &["search"]).stdout.contains(&app));

    // --- Updating ---------------------------------------------------------------------
    owner.ok(
        cwd,
        &[
            "publish",
            package(&sources, &base, "1.1.0", &[], "base one-one\n")
                .to_str()
                .unwrap(),
        ],
    );
    assert!(
        reader
            .ok(&project, &["install"])
            .stdout
            .contains("Already up to date"),
        "the lockfile pins 1.0.0"
    );
    assert!(
        reader
            .ok(&project, &["outdated"])
            .stdout
            .contains(&format!("{base} 1.0.0 -> 1.1.0"))
    );
    assert!(
        reader
            .ok(&project, &["update"])
            .stdout
            .contains(&format!("{base} 1.0.0 -> 1.1.0"))
    );
    assert_eq!(
        read(project.join(format!(".ctx/packages/{base}/docs/body.md"))),
        "base one-one\n"
    );
    assert!(
        reader
            .ok(&project, &["outdated"])
            .stdout
            .contains("newest version")
    );

    // A fresh checkout with the same lockfile gets the same versions.
    let checkout = work.path().join("checkout");
    fs::create_dir_all(&checkout).unwrap();
    fs::copy(project.join("ctx.toml"), checkout.join("ctx.toml")).unwrap();
    fs::copy(project.join("ctx.lock"), checkout.join("ctx.lock")).unwrap();
    reader.ok(&checkout, &["install", "--frozen"]);
    assert_eq!(
        read(checkout.join(format!(".ctx/packages/{base}/docs/body.md"))),
        "base one-one\n"
    );

    // --- Yanking ----------------------------------------------------------------------
    owner.ok(cwd, &["yank", &format!("{base}@1.1.0")]);
    let fresh = work.path().join("fresh");
    fs::create_dir_all(&fresh).unwrap();
    assert!(
        reader
            .ok(&fresh, &["install", &app])
            .stdout
            .contains(&format!("+ {base} 1.0.0")),
        "a yanked version is not selected"
    );
    assert!(
        reader
            .ok(&project, &["install"])
            .stdout
            .contains("Already up to date"),
        "a pinned yanked version stays"
    );
    assert!(
        reader
            .fails(cwd, &["yank", &format!("{base}@1.0.0")])
            .contains("not allowed")
    );
    owner.ok(cwd, &["yank", &format!("{base}@1.1.0"), "--undo"]);

    // --- A dependency the reader cannot see -------------------------------------------
    owner.ok(
        cwd,
        &[
            "publish",
            package(&sources, &secret, "1.0.0", &[], "secret\n")
                .to_str()
                .unwrap(),
        ],
    );
    owner.ok(
        cwd,
        &[
            "publish",
            package(
                &sources,
                &bundle,
                "1.0.0",
                &[(&secret, "*"), (&base, "*")],
                "bundle\n",
            )
            .to_str()
            .unwrap(),
        ],
    );
    owner.ok(cwd, &["access", "grant", &bundle, &reader_email]);
    let partial = work.path().join("partial");
    fs::create_dir_all(&partial).unwrap();
    let strict = reader.fails(&partial, &["install", &bundle, "--strict"]);
    assert!(strict.contains(&secret), "{strict}");
    let lenient = reader.ok(&partial, &["install", &bundle]);
    assert!(
        lenient.stderr.contains(&format!("left out \"{secret}\"")),
        "{}",
        lenient.all()
    );
    assert!(partial.join(format!(".ctx/packages/{bundle}")).is_dir());
    assert!(!partial.join(format!(".ctx/packages/{secret}")).exists());

    // --- A custom install directory with preserved local files -------------------------
    let home = work.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("ctx.toml"),
        "[install]\ndir = \"layers\"\npreserve = [\".local\"]\n",
    )
    .unwrap();
    reader.ok(&home, &["install", &format!("{base}@1.0.0")]);
    assert!(home.join(format!("layers/{base}/manifest.json")).is_file());
    fs::create_dir_all(home.join(format!("layers/{base}/.local"))).unwrap();
    fs::write(home.join(format!("layers/{base}/.local/notes.md")), "mine").unwrap();
    reader.ok(&home, &["install", &format!("{base}@^1.1.0")]);
    assert_eq!(
        read(home.join(format!("layers/{base}/docs/body.md"))),
        "base one-one\n"
    );
    assert_eq!(
        read(home.join(format!("layers/{base}/.local/notes.md"))),
        "mine"
    );
    assert!(
        read(home.join("ctx.toml")).contains("dir = \"layers\""),
        "install settings survive a rewrite of ctx.toml"
    );

    // --- Uninstalling -----------------------------------------------------------------
    let removed = reader.ok(&project, &["uninstall", &app]).stdout;
    assert!(
        removed.contains(&format!("- {app}")) && removed.contains(&format!("- {base}")),
        "{removed}"
    );
    assert!(!project.join(format!(".ctx/packages/{app}")).exists());
    assert!(!project.join(format!(".ctx/packages/{base}")).exists());
    assert!(
        reader
            .fails(&project, &["uninstall", &app])
            .contains("not listed in ctx.toml")
    );

    // --- Public packages need no sign-in ------------------------------------------------
    owner.ok(
        cwd,
        &[
            "create",
            &open,
            "--type",
            "project",
            "--public",
            "--description",
            "open to all",
        ],
    );
    owner.ok(
        cwd,
        &[
            "publish",
            package(&sources, &open, "1.0.0", &[], "open\n")
                .to_str()
                .unwrap(),
        ],
    );
    let public = work.path().join("public");
    fs::create_dir_all(&public).unwrap();
    anonymous.ok(&public, &["install", &open]);
    assert_eq!(
        read(public.join(format!(".ctx/packages/{open}/docs/body.md"))),
        "open\n"
    );
    assert!(anonymous.ok(cwd, &["search"]).stdout.contains(&open));
    owner.ok(cwd, &["access", "visibility", &open, "restricted"]);
    assert!(
        anonymous
            .fails(&public, &["info", &open])
            .contains("was not found")
    );

    // --- An expired session is refreshed without a new login ---------------------------
    let credentials_path = reader.config.path().join("credentials.json");
    let mut credentials: Value = serde_json::from_str(&read(credentials_path.clone())).unwrap();
    let old_token = credentials["sessions"]["local"]["id_token"]
        .as_str()
        .unwrap()
        .to_string();
    credentials["sessions"]["local"]["expires_at"] = json!(0);
    credentials["sessions"]["local"]["id_token"] = json!("expired");
    fs::write(&credentials_path, credentials.to_string()).unwrap();
    assert!(reader.ok(cwd, &["whoami"]).stdout.contains(&reader_email));
    let refreshed: Value = serde_json::from_str(&read(credentials_path)).unwrap();
    assert_ne!(refreshed["sessions"]["local"]["id_token"], "expired");
    assert!(
        refreshed["sessions"]["local"]["expires_at"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(!old_token.is_empty());

    reader.ok(cwd, &["logout"]);
    assert!(reader.ok(cwd, &["whoami"]).stdout.contains("Not signed in"));
}
