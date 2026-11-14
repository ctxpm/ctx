# Package format

A ctx package is a gzip-compressed tar archive (`.tgz`) of a directory with a
`manifest.json` at its root. Everything else in the directory is content and is not
interpreted by ctx.

## Manifest

Schema: [`spec/manifest.schema.json`](../spec/manifest.schema.json).

| Field | Required | Meaning |
|---|---|---|
| `schemaVersion` | yes | Always `1` |
| `name` | yes | Lowercase kebab-case, at most 64 characters |
| `version` | yes | Exact semantic version, no leading `v` |
| `type` | yes | Kind of context, for example `project`; fixed per package name |
| `description` | no | One line, at most 500 characters |
| `dependencies` | no | Package name → semver range |
| `provides`, `engines`, `provenance` | no | Free-form; stored and passed through |

Unknown top-level fields are allowed and preserved.

## Archive rules

- `manifest.json` sits at the archive root. An archive in which every entry lives under one
  single top-level directory (as produced by `npm pack`) is also accepted; that directory
  is stripped on install.
- Only regular files and directories. Symbolic links, hard links and device entries are
  rejected.
- No absolute paths and no `..` segments.
- A `README.md` at the package root is shown on the package page of the registry.

## Versions are immutable

Once `name@version` is published it can never be replaced. A version can be **yanked**:
it stays downloadable for anyone who pinned it, but it is no longer selected when a range
is resolved.

## Integrity

The registry records the SHA-256 digest of the archive as `sha256-<hex>`. Clients verify
the digest before extracting anything.
