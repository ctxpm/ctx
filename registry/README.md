# Registry backend

The registry is a Firebase project with four parts:

| Part | Role |
|---|---|
| Firestore | Index: one document per package and per version, plus who has access |
| Cloud Storage | Artifacts: `packages/<name>/<version>/<sha256>.tgz` |
| Authentication | Google sign-in; identity is the verified email address |
| Cloud Function `api` | The only way to write: create packages, publish, yank |

Reads never go through the function. Clients read Firestore and download from Storage
directly, and [`firestore.rules`](firestore.rules) and [`storage.rules`](storage.rules)
decide what each caller may see.

## Access model

Access to a package is kept apart from the package itself:

| Document | Holds | Who can read it |
|---|---|---|
| `packages/{name}` | type, description, visibility, dist-tags, counts | anyone entitled to the package |
| `packages/{name}/private/access` | `visibility`, `readers`, `readerDomains`, `maintainers`, `trustedPublishers` | maintainers of that package |
| `grants/{package}\|{kind}\|{who}` | one entry per reader, domain or maintainer | only the person or domain it names |

- `visibility` is `public` (anyone, signed in or not) or `restricted`.
- `readers` and `readerDomains` are the email addresses and email domains that may read a
  restricted package. `maintainers` may publish, yank and change access.
- A reader does not see who else has access, or who maintains a package.
- A package you cannot read does not show up in any query you are allowed to run. A client
  finds what is shared with it through its own grants, then reads those packages.
- Nothing is writable by clients. Every change goes through the write API, which validates
  and normalises it and keeps the grants in line with the access document.

Who may claim a **new** package name is set in the document `registry/config`, which only
the function can read:

```json
{ "creators": ["you@example.dev"], "creatorDomains": ["example.dev"] }
```

Without that document nobody can create packages.

## Write API

Base path `/v1`. Every call needs `Authorization: Bearer <token>`: a Firebase ID token of a
signed-in user, or an OIDC token from GitHub Actions requested for the registry's audience.

| Call | Who | Effect |
|---|---|---|
| `GET /health` | anyone | `{ "ok": true }` |
| `POST /packages` | creators | Claims a name and sets its access lists (JSON body) |
| `POST /publish[?tag=<name>]` | maintainers, trusted publishers | Body is the `.tgz`; name and version come from its `manifest.json`. A creator publishing an unknown name creates the package as `restricted` |
| `POST /packages/<name>/versions/<version>/yank` | maintainers | Body `{"yanked": true}` or `false` |
| `PATCH /packages/<name>/access` | maintainers | Replaces any of `visibility`, `readers`, `readerDomains`, `maintainers`, `trustedPublishers`; what is left out stays as it is |

Publishing a version that already exists returns `409`; versions are immutable. Without
`?tag`, `latest` follows the highest stable version that is not yanked.

### Trusted publishers

```json
{
  "provider": "github",
  "repository": "owner/repo",
  "workflow": "owner/repo/.github/workflows/release.yml",
  "ref": "refs/heads/main",
  "environment": "release"
}
```

Only `provider` and `repository` are required. Every field that is set must match the
token of the publishing job. Without `ref` and `workflow`, any workflow on any branch of
the repository can publish, so pin both: `ctx access trust <package> owner/repo
--workflow release.yml --ref refs/heads/main`. A CI job can publish new versions of a package that trusts
it; it can never create a package or yank a version.

## Settings

Environment variables of the function, for example in `functions/.env.<project id>`:

| Variable | Default | Meaning |
|---|---|---|
| `CTX_REGION` | `europe-west4` | Region the function is deployed to |
| `CTX_BUCKET` | default bucket | Bucket that holds the artifacts |
| `CTX_MAX_ARTIFACT_MB` | `20` | Largest archive the publish call accepts |
| `CTX_OIDC_AUDIENCE` | `ctx:<project id>` | Audience a CI job must request |
| `CTX_OIDC_ISSUER`, `CTX_OIDC_JWKS_URL` | GitHub Actions | Issuer of CI tokens and its key set |

## Tests

```sh
cd functions
npm install
npm test
```

`npm test` typechecks, runs the unit tests, then starts the Auth, Firestore, Storage and
Functions emulators under the throwaway project id `demo-ctx` and runs the rules tests and
the API tests against them. Nothing touches a real Firebase project.

## Deploying to your own project

1. Create a Firebase project on the Blaze plan with Firestore, Cloud Storage and
   Authentication (Google sign-in enabled), and register a web app in it.
2. In the Google Cloud console of that project, create an OAuth client of type
   "Desktop app". The CLI signs in with it.
3. Write `registry/descriptor.<project id>.json` (shape: `spec/registry.schema.json`) with
   the web app's `apiKey`, the bucket, the address of the site plus `/v1` as `api`, and
   the desktop client under `oauth`. The file is public once deployed, but it is not
   committed: it belongs to your deployment.
4. Deploy:

   ```sh
   scripts/deploy-registry.sh <project id>
   ```

   This publishes the rules, the `api` function and the web app, and serves the
   descriptor at `/.well-known/ctx-registry.json`.
5. Create the document `registry/config` in Firestore with yourself as a creator.
6. Let Cloud Storage rules read Firestore, once per project:

   ```sh
   gcloud projects add-iam-policy-binding <project id> \
     --member "serviceAccount:service-<project number>@gcp-sa-firebasestorage.iam.gserviceaccount.com" \
     --role roles/firebaserules.firestoreServiceAgent
   ```

People then run `ctx registry add https://<your site>`.

### Upgrading a registry from before 0.2.0

Access lists used to live on the package document. After deploying 0.2.0, move them once:

```sh
cd registry/functions
GCLOUD_PROJECT=<project id> node scripts/migrate-access.mjs           # shows what would change
GCLOUD_PROJECT=<project id> node scripts/migrate-access.mjs --apply
```

Clients older than 0.2.0 only see public packages afterwards.
