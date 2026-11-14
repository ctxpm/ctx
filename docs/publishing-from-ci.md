# Publishing from GitHub Actions

A package can trust a GitHub repository. A workflow in that repository then publishes new
versions with its own identity: there is no token to create, store or rotate.

## 1. Trust the repository

As a maintainer of the package:

```sh
ctx access trust team-handbook acme/handbook --workflow release.yml --ref refs/heads/main
```

`--workflow` and `--ref` limit which jobs may publish. Leave them out and any workflow on
any branch of the repository can publish, including one in a pull request branch, so pin
both. A CI job can only add versions; it cannot create a package, yank a version or change
who has access.

## 2. Publish from a workflow

```yaml
name: Release
on:
  push:
    branches: [main]

jobs:
  publish:
    runs-on: ubuntu-latest
    permissions:
      id-token: write   # lets the job prove which repository and workflow it is
      contents: read
    steps:
      - uses: actions/checkout@v4
      - uses: ctxpm/ctx/actions/publish@main
        with:
          registry: https://registry.example.dev
          path: .
```

| Input | Default | Meaning |
|---|---|---|
| `registry` | (required) | Address of the registry |
| `path` | `.` | Directory with the package's `manifest.json` |
| `tag` | | Publish under this dist-tag instead of moving `latest` |
| `ctx-version` | `latest` | Version of the CLI to use |

Publishing a version that already exists fails; bump `version` in `manifest.json` first.

## Using the reusable workflow

`ctxpm/ctx/.github/workflows/publish.yml` wraps the same steps. The registry then sees
that file as the workflow, so trust it by its full path:

```sh
ctx access trust team-handbook acme/handbook \
  --workflow ctxpm/ctx/.github/workflows/publish.yml --ref refs/heads/main
```

```yaml
jobs:
  publish:
    permissions:
      id-token: write
      contents: read
    uses: ctxpm/ctx/.github/workflows/publish.yml@main
    with:
      registry: https://registry.example.dev
```

## How it works

The action asks GitHub for an identity token issued for the audience the registry
publishes in `/.well-known/ctx-registry.json`. The registry verifies the token against
GitHub's keys and compares its repository, workflow and ref with what the package trusts.
The version records where it came from: repository, workflow, ref, commit and run.
