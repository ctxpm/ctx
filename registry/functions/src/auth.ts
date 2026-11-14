import type { Auth } from 'firebase-admin/auth';
import { createRemoteJWKSet, decodeJwt, jwtVerify, type JWTVerifyGetKey } from 'jose';
import { ApiError } from './errors.js';

/** A person signed in with a verified email address. */
export interface UserCaller {
  kind: 'user';
  uid: string;
  email: string;
}

/** A CI job that proved its identity with an OIDC token from GitHub Actions. */
export interface GithubCaller {
  kind: 'github';
  /** "owner/repo", lowercase. */
  repository: string;
  /** "owner/repo/.github/workflows/file.yml", lowercase, without the "@ref" suffix. */
  workflow: string | null;
  ref: string | null;
  environment: string | null;
  sha: string | null;
  runId: string | null;
}

export type Caller = UserCaller | GithubCaller;

export interface OidcConfig {
  issuer: string;
  jwksUrl: string;
  audience: string;
}

export type Identify = (authorization: string | undefined) => Promise<Caller>;

function claim(payload: Record<string, unknown>, name: string): string | null {
  const value = payload[name];
  return typeof value === 'string' && value !== '' ? value : null;
}

/**
 * Builds the function that turns an Authorization header into a caller. A token issued by
 * the configured CI issuer is verified against that issuer's keys; anything else must be a
 * Firebase ID token.
 */
export function createIdentify(auth: Auth, oidc: OidcConfig): Identify {
  let keys: JWTVerifyGetKey | undefined;

  return async (authorization) => {
    const match = /^Bearer\s+(\S+)$/i.exec(authorization ?? '');
    if (!match?.[1]) {
      throw new ApiError(401, 'unauthenticated', 'Missing bearer token');
    }
    const token = match[1];

    let issuer: string | undefined;
    try {
      issuer = decodeJwt(token).iss;
    } catch {
      throw new ApiError(401, 'unauthenticated', 'Token is not a JWT');
    }

    if (issuer === oidc.issuer) {
      keys ??= createRemoteJWKSet(new URL(oidc.jwksUrl));
      let payload: Record<string, unknown>;
      try {
        ({ payload } = await jwtVerify(token, keys, { issuer: oidc.issuer, audience: oidc.audience }));
      } catch (error) {
        throw new ApiError(401, 'unauthenticated', `CI token rejected: ${(error as Error).message}`);
      }
      const repository = claim(payload, 'repository');
      if (repository === null) {
        throw new ApiError(401, 'unauthenticated', 'CI token has no repository claim');
      }
      const workflowRef = claim(payload, 'job_workflow_ref');
      return {
        kind: 'github',
        repository: repository.toLowerCase(),
        workflow: workflowRef === null ? null : (workflowRef.split('@')[0] ?? '').toLowerCase(),
        ref: claim(payload, 'ref'),
        environment: claim(payload, 'environment'),
        sha: claim(payload, 'sha'),
        runId: claim(payload, 'run_id'),
      };
    }

    let decoded;
    try {
      decoded = await auth.verifyIdToken(token);
    } catch {
      throw new ApiError(401, 'unauthenticated', 'Sign-in token rejected');
    }
    if (!decoded.email || decoded.email_verified !== true) {
      throw new ApiError(403, 'email_not_verified', 'A verified email address is required');
    }
    return { kind: 'user', uid: decoded.uid, email: decoded.email.toLowerCase() };
  };
}
