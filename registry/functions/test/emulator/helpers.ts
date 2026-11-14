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

/** Writes a package with its access document and grants directly, bypassing the API. */
export async function seedPackage(name: string, access: SeedAccess = {}): Promise<void> {
  const visibility = access.visibility ?? 'restricted';
  const readers = access.readers ?? [];
  const readerDomains = access.readerDomains ?? [];
  const maintainers = access.maintainers ?? ['owner@example.dev'];
  const batch = adminDb.batch();
  batch.set(adminDb.doc(`packages/${name}`), {
    name,
    type: 'project',
    description: `${name} seeded`,
    visibility,
    distTags: {},
    latestVersion: null,
    versionCount: 0,
  });
  batch.set(adminDb.doc(`packages/${name}/private/access`), {
    visibility,
    readers,
    readerDomains,
    maintainers,
    trustedPublishers: access.trustedPublishers ?? [],
  });
  const grants = [
    ...readers.map((principal) => ({ kind: 'reader', principal })),
    ...maintainers.map((principal) => ({ kind: 'maintainer', principal })),
    ...readerDomains.map((principal) => ({ kind: 'domain', principal })),
  ];
  for (const grant of grants) {
    batch.set(adminDb.doc(`grants/${name}|${grant.kind}|${grant.principal}`), { package: name, ...grant });
  }
  await batch.commit();
}

/** The access document of a package, read with administrator rights. */
export async function accessOf(name: string): Promise<Record<string, any> | undefined> {
  return (await adminDb.doc(`packages/${name}/private/access`).get()).data();
}

/** The ids of the grants of a package, sorted. */
export async function grantsOf(name: string): Promise<string[]> {
  const snapshot = await adminDb.collection('grants').where('package', '==', name).get();
  return snapshot.docs.map((entry) => entry.id).sort();
}
