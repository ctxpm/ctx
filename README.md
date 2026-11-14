# ctx

A package manager for **context artifacts**: versioned bundles of Markdown and YAML that
give people and AI agents the same source of truth about a project, a team or a discipline.

> **Status: early development.** The registry backend is being built first; the CLI and
> the web UI follow. Nothing here is released yet.

## Why

Context is usually copied between repositories or shipped through a language package
manager it has nothing to do with. `ctx` treats a context bundle as a first-class package:

- **Typed and versioned.** Every package has a type (`project`, `discipline`, …), a semver
  version and dependencies on other packages.
- **Access per package.** A package is public or restricted to named readers and domains.
  A package you cannot read is invisible to you.
- **No runtime to install.** The CLI is a single native binary. It talks to the registry
  over HTTPS and does not need Node, npm or any other package manager.
- **Self-hostable.** The registry runs on Firebase (Storage, Firestore, Authentication, one
  Cloud Function). Nothing is tied to a particular Firebase project.

## Repository layout

| Path | Contents |
|---|---|
| `spec/` | JSON Schemas for the package manifest and the registry descriptor |
| `registry/` | Firebase backend: security rules, publish API, emulator tests |
| `crates/` | Rust CLI (planned) |
| `web/` | Web UI for browsing packages (planned) |
| `examples/` | Sample packages, also used as test fixtures |
| `docs/` | Design notes |

## A package

A package is a directory with a `manifest.json` at its root:

```json
{
  "schemaVersion": 1,
  "name": "team-handbook",
  "version": "1.0.0",
  "type": "team",
  "description": "How this team works",
  "dependencies": { "frontend-standards": "^1.0.0" }
}
```

Everything else in the directory is content. See [`docs/package-format.md`](docs/package-format.md).

## Development

The registry backend needs Node 24, Java 21+ (for the Firebase emulators) and the
Firebase CLI.

```sh
cd registry/functions
npm install
npm test          # unit tests, then rules and API tests against the emulators
```

## License

[Apache-2.0](LICENSE)
