// Everything the pages know about the registry: who is signed in and how to read and
// change packages. Reads go straight to Firestore under the registry's security rules,
// exactly like the CLI; this app has no server of its own.

import { initializeApp } from 'firebase/app';
import {
  connectAuthEmulator,
  getAuth,
  GoogleAuthProvider,
  onAuthStateChanged,
  signInWithCredential,
  signInWithPopup,
  signOut as firebaseSignOut,
  type Auth,
} from 'firebase/auth';
import {
  collection,
  connectFirestoreEmulator,
  doc,
  getDoc,
  getDocs,
  getFirestore,
  query,
  where,
  type Firestore,
  type Timestamp,
} from 'firebase/firestore';

interface Descriptor {
  name?: string;
  firebase: { projectId: string; apiKey: string; storageBucket: string; authDomain?: string };
  api: string;
  endpoints?: { identityToolkit?: string; firestore?: string };
}

export interface TrustedPublisher {
  provider: 'github';
  repository: string;
  workflow?: string;
  ref?: string;
  environment?: string;
}

export interface Package {
  name: string;
  type: string;
  description: string;
  visibility: 'public' | 'restricted';
  latest: string | null;
  distTags: Record<string, string>;
  versionCount: number;
}

/** Who may read, maintain and publish a package. Only its maintainers receive this. */
export interface Access {
  visibility: 'public' | 'restricted';
  readers: string[];
  readerDomains: string[];
  maintainers: string[];
  trustedPublishers: TrustedPublisher[];
}

export interface Version {
  version: string;
  description: string;
  dependencies: Record<string, string>;
  integrity: string;
  size: number;
  fileCount: number;
  files: { path: string; size: number }[];
  readme: string | null;
  yanked: boolean;
  publishedAt: Date | null;
  publishedBy: string;
}

/** Who is using the app. `ready` turns true once the registry answered and sign-in state is known. */
export const session = $state({
  ready: false,
  registryName: 'ctx registry',
  email: null as string | null,
  /** Why the app cannot work at all, for example a missing descriptor. */
  failure: null as string | null,
});

let auth: Auth | undefined;
let db: Firestore | undefined;
let api = '';

/** Reads the registry descriptor and connects to the registry it describes. */
export async function connect(): Promise<void> {
  try {
    const response = await fetch('/.well-known/ctx-registry.json', { cache: 'no-cache' });
    if (!response.ok) {
      throw new Error(`/.well-known/ctx-registry.json returned ${response.status}`);
    }
    const descriptor = (await response.json()) as Descriptor;
    const app = initializeApp({
      apiKey: descriptor.firebase.apiKey,
      projectId: descriptor.firebase.projectId,
      storageBucket: descriptor.firebase.storageBucket,
      authDomain: descriptor.firebase.authDomain ?? `${descriptor.firebase.projectId}.firebaseapp.com`,
    });
    auth = getAuth(app);
    db = getFirestore(app);
    api = descriptor.api.replace(/\/$/, '');
    session.registryName = descriptor.name ?? 'ctx registry';

    // A descriptor with endpoint overrides describes a registry on the local emulators.
    const firestoreUrl = descriptor.endpoints?.firestore;
    if (firestoreUrl) {
      const url = new URL(firestoreUrl);
      connectFirestoreEmulator(db, url.hostname, Number(url.port));
    }
    const identityUrl = descriptor.endpoints?.identityToolkit;
    if (identityUrl) {
      connectAuthEmulator(auth, new URL(identityUrl).origin, { disableWarnings: true });
      // The emulator accepts an unsigned identity, which lets tests sign in without the
      // pop-up. Real Google sign-in rejects it, and this hook only exists on emulators.
      const emulated = auth;
      (window as unknown as Record<string, unknown>).__ctxDevSignIn = (email: string) =>
        signInWithCredential(
          emulated,
          GoogleAuthProvider.credential(JSON.stringify({ sub: `dev-${email}`, email, email_verified: true })),
        );
    }

    onAuthStateChanged(auth, (user) => {
      session.email = user?.email && user.emailVerified ? user.email.toLowerCase() : null;
      session.ready = true;
    });
  } catch (error) {
    session.failure = `This site could not reach its registry: ${(error as Error).message}`;
    session.ready = true;
  }
}

export async function signIn(): Promise<void> {
  if (auth) {
    const provider = new GoogleAuthProvider();
    provider.setCustomParameters({ prompt: 'select_account' });
    await signInWithPopup(auth, provider);
  }
}

export async function signOut(): Promise<void> {
  if (auth) {
    await firebaseSignOut(auth);
  }
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : [];
}

function toPackage(data: Record<string, unknown>): Package {
  const distTags = (data.distTags ?? {}) as Record<string, string>;
  return {
    name: String(data.name ?? ''),
    type: String(data.type ?? ''),
    description: String(data.description ?? ''),
    visibility: data.visibility === 'public' ? 'public' : 'restricted',
    latest: distTags.latest ?? null,
    distTags,
    versionCount: Number(data.versionCount ?? 0),
  };
}

function toAccess(data: Record<string, unknown>): Access {
  return {
    visibility: data.visibility === 'public' ? 'public' : 'restricted',
    readers: strings(data.readers),
    readerDomains: strings(data.readerDomains),
    maintainers: strings(data.maintainers),
    trustedPublishers: Array.isArray(data.trustedPublishers) ? (data.trustedPublishers as TrustedPublisher[]) : [],
  };
}

/**
 * Every package the visitor may read: the public ones, plus what is shared with them. The
 * registry shows a visitor only the grants that name their address or their domain.
 */
export async function listPackages(): Promise<Package[]> {
  if (!db) {
    return [];
  }
  const found = new Map<string, Package>();
  const publicOnes = await getDocs(query(collection(db, 'packages'), where('visibility', '==', 'public')));
  for (const entry of publicOnes.docs) {
    found.set(entry.id, toPackage(entry.data()));
  }

  const email = session.email;
  if (email) {
    const grants = collection(db, 'grants');
    const snapshots = await Promise.all([
      getDocs(query(grants, where('kind', 'in', ['reader', 'maintainer']), where('principal', '==', email))),
      getDocs(query(grants, where('kind', '==', 'domain'), where('principal', '==', email.split('@')[1] ?? ''))),
    ]);
    const names = new Set(snapshots.flatMap((snapshot) => snapshot.docs.map((entry) => String(entry.data().package))));
    const shared = await Promise.all([...names].filter((name) => !found.has(name)).map((name) => getPackage(name)));
    for (const entry of shared) {
      if (entry) {
        found.set(entry.name, entry);
      }
    }
  }
  return [...found.values()].sort((a, b) => a.name.localeCompare(b.name));
}

/** The package, or null when it does not exist or the visitor may not see it. */
export async function getPackage(name: string): Promise<Package | null> {
  if (!db) {
    return null;
  }
  try {
    const snapshot = await getDoc(doc(db, 'packages', name));
    return snapshot.exists() ? toPackage(snapshot.data()) : null;
  } catch {
    return null;
  }
}

/** Who has access to a package, or null when the visitor is not one of its maintainers. */
export async function getAccess(name: string): Promise<Access | null> {
  if (!db || !session.email) {
    return null;
  }
  try {
    const snapshot = await getDoc(doc(db, 'packages', name, 'private', 'access'));
    return snapshot.exists() ? toAccess(snapshot.data()) : null;
  } catch {
    return null;
  }
}

/** Compares two semantic versions well enough to sort a version list, newest first. */
function newestFirst(a: string, b: string): number {
  const parse = (version: string) => {
    const [core = '', pre] = version.split('+')[0]!.split(/-(.*)/s);
    return { numbers: core.split('.').map(Number), pre: pre ?? '' };
  };
  const left = parse(a);
  const right = parse(b);
  for (let index = 0; index < 3; index++) {
    const difference = (right.numbers[index] ?? 0) - (left.numbers[index] ?? 0);
    if (difference !== 0) {
      return difference;
    }
  }
  // A release sorts above its own pre-releases.
  if (left.pre === right.pre) {
    return 0;
  }
  if (left.pre === '') {
    return -1;
  }
  if (right.pre === '') {
    return 1;
  }
  return right.pre.localeCompare(left.pre, undefined, { numeric: true });
}

export async function getVersions(name: string): Promise<Version[]> {
  if (!db) {
    return [];
  }
  const snapshot = await getDocs(collection(db, 'packages', name, 'versions'));
  return snapshot.docs
    .map((entry) => {
      const data = entry.data();
      const by = (data.publishedBy ?? {}) as { email?: string; repository?: string };
      return {
        version: String(data.version ?? entry.id),
        description: String(data.description ?? ''),
        dependencies: (data.dependencies ?? {}) as Record<string, string>,
        integrity: String(data.integrity ?? ''),
        size: Number(data.size ?? 0),
        fileCount: Number(data.fileCount ?? 0),
        files: Array.isArray(data.files) ? (data.files as { path: string; size: number }[]) : [],
        readme: typeof data.readme === 'string' ? data.readme : null,
        yanked: data.yanked === true,
        publishedAt: (data.publishedAt as Timestamp | undefined)?.toDate?.() ?? null,
        publishedBy: by.email ?? by.repository ?? '',
      };
    })
    .sort((a, b) => newestFirst(a.version, b.version));
}

export type AccessChange = Partial<Access>;

/** Calls the registry's write API as the signed-in visitor. */
async function callApi(method: string, path: string, body: unknown): Promise<void> {
  const token = await auth?.currentUser?.getIdToken();
  if (!token) {
    throw new Error('Sign in first');
  }
  const response = await fetch(`${api}${path}`, {
    method,
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const answer = (await response.json().catch(() => null)) as { error?: { message?: string } } | null;
    throw new Error(answer?.error?.message ?? `The registry returned ${response.status}`);
  }
}

/** Changes who has access to a package. The registry only allows this to maintainers. */
export function updateAccess(name: string, change: AccessChange): Promise<void> {
  return callApi('PATCH', `/packages/${encodeURIComponent(name)}/access`, change);
}

/** Yanks a version, or restores it. */
export function setYanked(name: string, version: string, yanked: boolean): Promise<void> {
  return callApi('POST', `/packages/${encodeURIComponent(name)}/versions/${encodeURIComponent(version)}/yank`, {
    yanked,
  });
}

/** Common package types keep a colour of their own; any other type gets one by hash. */
const KNOWN_HUES: Record<string, number> = {
  discipline: 1,
  project: 2,
  market: 3,
  team: 4,
  client: 5,
  vertical: 6,
};

/** One of six layer colours, fixed per package type, so a type always looks the same. */
export function hueOf(type: string): string {
  let index = KNOWN_HUES[type];
  if (index === undefined) {
    let hash = 0;
    for (const character of type) {
      hash = (hash * 31 + character.charCodeAt(0)) >>> 0;
    }
    index = (hash % 6) + 1;
  }
  return `var(--stratum-${index})`;
}
