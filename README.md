# ctx

A package manager for **context artifacts**: versioned bundles of Markdown and YAML that
give people and AI agents the same source of truth about a project, a team or a discipline.

> **Status: early development.** Registry backend, CLI and web app work end to end against
> the Firebase emulators. Expect breaking changes.

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
| `crates/` | Rust CLI: `ctx-core` (library) and `ctx-cli` (the `ctx` binary) |
| `web/` | Web app for browsing packages and managing access (Svelte) |
| `actions/publish/` | GitHub Action that publishes a package from CI |
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

## Installing the CLI

```sh
curl -fsSL https://raw.githubusercontent.com/ctxpm/ctx/main/install.sh | sh
```

This downloads the binary for your platform from the GitHub releases, checks its SHA-256
and puts it in `~/.local/bin`. It needs no Node, npm or other package manager. Windows
binaries are on the [releases page](https://github.com/ctxpm/ctx/releases).

## Using the CLI

```sh
ctx registry add https://registry.example.dev   # reads /.well-known/ctx-registry.json
ctx login                                        # Google sign-in in your browser

ctx install team-handbook                        # adds it to ctx.toml, writes ctx.lock
ctx install                                      # installs what ctx.toml and ctx.lock say
ctx update                                       # newest versions the ranges allow
ctx list
ctx search handbook

ctx publish ./team-handbook                      # a directory with a manifest.json
ctx access grant team-handbook ana@example.dev @example.org
ctx access trust team-handbook acme/handbook --workflow release.yml --ref refs/heads/main
```

Packages land in `.ctx/packages/<name>/`. Both the directory and the paths inside a
package that must survive an update are settings in `ctx.toml`:

```toml
[packages]
team-handbook = "^1.0.0"

[install]
dir = "layers"
preserve = [".local", "*.local.yml"]
```

To publish from GitHub Actions without a stored secret, see
[`docs/publishing-from-ci.md`](docs/publishing-from-ci.md).

## Development

Needs Rust (stable), Node 24, Java 21+ (for the Firebase emulators) and the Firebase CLI.

```sh
cargo test                               # CLI and core unit tests

cd registry/functions && npm install
npm test                                 # registry: typecheck, unit, rules and API tests

./scripts/e2e.sh                         # the ctx binary against the emulators
```

The web app reads its whole configuration from `/.well-known/ctx-registry.json` at
runtime. `npm run dev` in `web/` serves the descriptor of the local emulators, so start
those first (`firebase emulators:start --project demo-ctx` in `registry/`).

To run your own registry, see [`registry/README.md`](registry/README.md).

Every test that needs a registry runs against the emulators under the throwaway project
id `demo-ctx`; nothing touches a real Firebase project.

## License

[Apache-2.0](LICENSE)
