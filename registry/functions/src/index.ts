import { initializeApp } from 'firebase-admin/app';
import { getAuth } from 'firebase-admin/auth';
import { getFirestore } from 'firebase-admin/firestore';
import { getStorage } from 'firebase-admin/storage';
import { logger } from 'firebase-functions';
import { onRequest } from 'firebase-functions/v2/https';
import { handle, type Deps } from './app.js';
import { createIdentify } from './auth.js';

// Deployment settings, read from the environment (for example registry/functions/.env or
// .env.<project id>). Every setting has a default, so a plain `firebase deploy` works.
const env = (name: string, fallback: string): string => {
  const value = process.env[name];
  return value === undefined || value === '' ? fallback : value;
};

const REGION = env('CTX_REGION', 'europe-west4');

initializeApp();

let cached: Deps | undefined;

function deps(): Deps {
  if (cached !== undefined) {
    return cached;
  }
  const projectId = process.env.GCLOUD_PROJECT ?? process.env.GOOGLE_CLOUD_PROJECT ?? '';
  // Empty means the project's default bucket.
  const bucketName = env('CTX_BUCKET', '');
  const maxArtifactMb = Number(env('CTX_MAX_ARTIFACT_MB', '20'));
  const created: Deps = {
    db: getFirestore(),
    bucket: bucketName === '' ? getStorage().bucket() : getStorage().bucket(bucketName),
    identify: createIdentify(getAuth(), {
      // Defaults accept publish tokens from GitHub Actions.
      issuer: env('CTX_OIDC_ISSUER', 'https://token.actions.githubusercontent.com'),
      jwksUrl: env('CTX_OIDC_JWKS_URL', 'https://token.actions.githubusercontent.com/.well-known/jwks'),
      // The audience a CI job must request for its token.
      audience: env('CTX_OIDC_AUDIENCE', `ctx:${projectId}`),
    }),
    maxArtifactBytes: (Number.isFinite(maxArtifactMb) && maxArtifactMb > 0 ? maxArtifactMb : 20) * 1024 * 1024,
    log: (message, error) => logger.error(message, error),
  };
  cached = created;
  return created;
}

/** The registry's write API. Reads go straight to Firestore and Storage under the security rules. */
export const api = onRequest(
  { region: REGION, cors: true, memory: '512MiB', timeoutSeconds: 120, invoker: 'public' },
  async (req, res) => {
    const result = await handle(
      {
        method: req.method,
        path: req.path,
        query: req.query as Record<string, unknown>,
        authorization: req.get('authorization'),
        body: req.rawBody ?? Buffer.alloc(0),
      },
      deps(),
    );
    res.status(result.status).json(result.body);
  },
);
