# Registry backend

The registry is a Firebase project with four parts:

| Part | Role |
|---|---|
| Firestore | Index: one document per package, one per version |
| Cloud Storage | Artifacts: `packages/<name>/<version>/<sha256>.tgz` |
| Authentication | Google sign-in; identity is the verified email address |
| Cloud Function `api` | The only way to write: create packages, publish, yank |

Reads never go through the function. Clients read Firestore and download from Storage
directly, and [`firestore.rules`](firestore.rules) and [`storage.rules`](storage.rules)
decide what each caller may see.

## Access model

A package document carries its own access lists:

| Field | Meaning |
|---|---|
| `visibility` | `public` (anyone, signed in or not) or `restricted` |
| `readers` | Email addresses that may read a restricted package |
| `readerDomains` | Email domains that may read a restricted package |
| `maintainers` | Email addresses that may publish, yank and edit the access lists |
| `trustedPublishers` | CI workflows that may publish without a personal credential |

Email addresses and domains are stored in lower case. A package you cannot read does not
show up in any query you are allowed to run.

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
token of the publishing job. A CI job can publish new versions of a package that trusts
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
