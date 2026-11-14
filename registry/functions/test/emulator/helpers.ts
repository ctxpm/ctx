import { randomUUID } from 'node:crypto';
import { getApps, initializeApp } from 'firebase-admin/app';
import { getFirestore } from 'firebase-admin/firestore';
import { getStorage } from 'firebase-admin/storage';

// Must match registry/firebase.json and registry/functions/.env.demo-ctx.
export const PROJECT = 'demo-ctx';
export const BUCKET = 'demo-ctx.firebasestorage.app';
export const AUTH_HOST = '127.0.0.1:9099';
export const FIRESTORE = { host: '127.0.0.1', port: 8085 };
export const STORAGE = { host: '127.0.0.1', port: 9199 };
export const API = `http://127.0.0.1:5001/${PROJECT}/europe-west4/api/v1`;

process.env.FIRESTORE_EMULATOR_HOST ??= `${FIRESTORE.host}:${FIRESTORE.port}`;
process.env.FIREBASE_AUTH_EMULATOR_HOST ??= AUTH_HOST;
process.env.FIREBASE_STORAGE_EMULATOR_HOST ??= `${STORAGE.host}:${STORAGE.port}`;

if (getApps().length === 0) {
  initializeApp({ projectId: PROJECT, storageBucket: BUCKET });
}

export const adminDb = getFirestore();
export const adminBucket = getStorage().bucket(BUCKET);

/** A package name that no other test uses. */
export function uniqueName(prefix: string): string {
  return `${prefix}-${randomUUID().slice(0, 8)}`;
}

/** Signs in to the Auth emulator as a Google user and returns a Firebase ID token. */
export async function signIn(email: string, verified = true): Promise<string> {
  const idToken = JSON.stringify({ sub: `sub-${email}`, email, email_verified: verified });
  const response = await fetch(
    `http://${AUTH_HOST}/identitytoolkit.googleapis.com/v1/accounts:signInWithIdp?key=fake-api-key`,
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        postBody: `id_token=${encodeURIComponent(idToken)}&providerId=google.com`,
        requestUri: 'http://localhost',
        returnIdpCredential: true,
        returnSecureToken: true,
      }),
    },
  );
  const body = (await response.json()) as { idToken?: string };
  if (!response.ok || !body.idToken) {
    throw new Error(`Auth emulator sign-in failed for ${email}: ${JSON.stringify(body)}`);
  }
  return body.idToken;
}

export interface SeedAccess {
  visibility?: 'public' | 'restricted';
  readers?: string[];
  readerDomains?: string[];
  maintainers?: string[];
  trustedPublishers?: unknown[];
}

/** Writes a package document directly, bypassing the API. */
export async function seedPackage(name: string, access: SeedAccess = {}): Promise<void> {
  await adminDb.doc(`packages/${name}`).set({
    name,
    type: 'project',
    description: `${name} seeded`,
    visibility: access.visibility ?? 'restricted',
    readers: access.readers ?? [],
    readerDomains: access.readerDomains ?? [],
    maintainers: access.maintainers ?? ['owner@example.dev'],
    trustedPublishers: access.trustedPublishers ?? [],
    distTags: {},
    latestVersion: null,
    versionCount: 0,
  });
}
