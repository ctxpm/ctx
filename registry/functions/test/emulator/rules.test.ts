import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  assertFails,
  assertSucceeds,
  initializeTestEnvironment,
  type RulesTestContext,
  type RulesTestEnvironment,
} from '@firebase/rules-unit-testing';
import { collection, deleteDoc, doc, getDoc, getDocs, query, setDoc, updateDoc, where } from 'firebase/firestore';
import { getBytes, listAll, ref, uploadBytes } from 'firebase/storage';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { BUCKET, FIRESTORE, PROJECT, STORAGE } from './helpers.js';

const registryDir = join(import.meta.dirname, '../../..');

const RESTRICTED = 'rules-restricted';
const PUBLIC = 'rules-public';
const ARTIFACT = '1.0.0/abc123.tgz';

let env: RulesTestEnvironment;

/** The people in the access matrix. */
let owner: RulesTestContext;
let reader: RulesTestContext;
let readerUpperCase: RulesTestContext;
let colleague: RulesTestContext;
let stranger: RulesTestContext;
let unverifiedReader: RulesTestContext;
let anonymous: RulesTestContext;

function packageData(name: string, visibility: 'public' | 'restricted') {
  return {
    name,
    type: 'project',
    description: 'rules fixture',
    visibility,
    readers: ['reader@example.dev'],
    readerDomains: ['acme.dev'],
    maintainers: ['owner@example.dev'],
    trustedPublishers: [],
    distTags: { latest: '1.0.0' },
    latestVersion: '1.0.0',
    versionCount: 1,
  };
}

beforeAll(async () => {
  env = await initializeTestEnvironment({
    projectId: PROJECT,
    firestore: { ...FIRESTORE, rules: readFileSync(join(registryDir, 'firestore.rules'), 'utf8') },
    storage: { ...STORAGE, rules: readFileSync(join(registryDir, 'storage.rules'), 'utf8') },
  });

  const user = (email: string, verified = true) =>
    env.authenticatedContext(`uid-${email.toLowerCase()}`, { email, email_verified: verified });
  owner = user('owner@example.dev');
  reader = user('reader@example.dev');
  readerUpperCase = user('Reader@Example.dev');
  colleague = user('someone@acme.dev');
  stranger = user('stranger@elsewhere.dev');
  unverifiedReader = user('reader@example.dev', false);
  anonymous = env.unauthenticatedContext();

  await env.withSecurityRulesDisabled(async (admin) => {
    const db = admin.firestore();
    const storage = admin.storage(`gs://${BUCKET}`);
    for (const [name, visibility] of [
      [RESTRICTED, 'restricted'],
      [PUBLIC, 'public'],
    ] as const) {
      await setDoc(doc(db, 'packages', name), packageData(name, visibility));
      await setDoc(doc(db, 'packages', name, 'versions', '1.0.0'), { name, version: '1.0.0', yanked: false });
      await uploadBytes(ref(storage, `packages/${name}/${ARTIFACT}`), new Uint8Array([1, 2, 3]));
    }
    await setDoc(doc(db, 'registry', 'config'), { creators: ['owner@example.dev'] });
  });
});

afterAll(async () => {
  await env.cleanup();
});

const packageDoc = (context: RulesTestContext, name: string) => doc(context.firestore(), 'packages', name);
const versionDoc = (context: RulesTestContext, name: string) =>
  doc(context.firestore(), 'packages', name, 'versions', '1.0.0');
const artifact = (context: RulesTestContext, name: string) =>
  ref(context.storage(`gs://${BUCKET}`), `packages/${name}/${ARTIFACT}`);

describe('reading a restricted package', () => {
  it.each([
    ['a maintainer', () => owner],
    ['a listed reader', () => reader],
    ['a listed reader whose token has an upper-case email', () => readerUpperCase],
    ['someone from a listed domain', () => colleague],
  ])('allows %s to read the package, its versions and its artifact', async (_label, who) => {
    await assertSucceeds(getDoc(packageDoc(who(), RESTRICTED)));
    await assertSucceeds(getDoc(versionDoc(who(), RESTRICTED)));
    const bytes = await assertSucceeds(getBytes(artifact(who(), RESTRICTED)));
    expect(new Uint8Array(bytes)).toEqual(new Uint8Array([1, 2, 3]));
  });

  it.each([
    ['a signed-in stranger', () => stranger],
    ['a listed reader whose email is not verified', () => unverifiedReader],
    ['someone who is not signed in', () => anonymous],
  ])('denies %s the package, its versions and its artifact', async (_label, who) => {
    await assertFails(getDoc(packageDoc(who(), RESTRICTED)));
    await assertFails(getDoc(versionDoc(who(), RESTRICTED)));
    await assertFails(getBytes(artifact(who(), RESTRICTED)));
  });
});

describe('reading a public package', () => {
  it.each([
    ['a signed-in stranger', () => stranger],
    ['someone who is not signed in', () => anonymous],
  ])('allows %s', async (_label, who) => {
    await assertSucceeds(getDoc(packageDoc(who(), PUBLIC)));
    await assertSucceeds(getDoc(versionDoc(who(), PUBLIC)));
    await assertSucceeds(getBytes(artifact(who(), PUBLIC)));
  });
});

describe('listing packages', () => {
  const packages = (context: RulesTestContext) => collection(context.firestore(), 'packages');

  it('lets a reader find packages shared with their email', async () => {
    const result = await assertSucceeds(
      getDocs(query(packages(reader), where('readers', 'array-contains', 'reader@example.dev'))),
    );
    expect(result.docs.map((entry) => entry.id)).toContain(RESTRICTED);
  });

  it('lets a colleague find packages shared with their domain', async () => {
    const result = await assertSucceeds(
      getDocs(query(packages(colleague), where('readerDomains', 'array-contains', 'acme.dev'))),
    );
    expect(result.docs.map((entry) => entry.id)).toContain(RESTRICTED);
  });

  it('lets a maintainer find their own packages', async () => {
    await assertSucceeds(getDocs(query(packages(owner), where('maintainers', 'array-contains', 'owner@example.dev'))));
  });

  it('lets anyone list public packages', async () => {
    const result = await assertSucceeds(getDocs(query(packages(anonymous), where('visibility', '==', 'public'))));
    expect(result.docs.map((entry) => entry.id)).toContain(PUBLIC);
  });

  it('denies listing everything', async () => {
    await assertFails(getDocs(packages(reader)));
    await assertFails(getDocs(packages(anonymous)));
  });

  it("denies querying with someone else's email or domain", async () => {
    await assertFails(getDocs(query(packages(stranger), where('readers', 'array-contains', 'reader@example.dev'))));
    await assertFails(getDocs(query(packages(stranger), where('readerDomains', 'array-contains', 'acme.dev'))));
    await assertFails(getDocs(query(packages(stranger), where('visibility', '==', 'restricted'))));
  });

  it('lets a reader list the versions of a package they can read, and nobody else', async () => {
    const versions = (context: RulesTestContext) => collection(context.firestore(), 'packages', RESTRICTED, 'versions');
    await assertSucceeds(getDocs(versions(reader)));
    await assertFails(getDocs(versions(stranger)));
  });
});

describe('changing a package', () => {
  it('lets a maintainer edit the access lists', async () => {
    await assertSucceeds(
      updateDoc(packageDoc(owner, RESTRICTED), { readers: ['reader@example.dev', 'second@example.dev'] }),
    );
    await assertSucceeds(updateDoc(packageDoc(owner, RESTRICTED), { readers: ['reader@example.dev'] }));
  });

  it.each([
    ['the name', { name: 'renamed' }],
    ['the type', { type: 'other' }],
    ['the dist-tags', { distTags: { latest: '9.9.9' } }],
    ['the version count', { versionCount: 99 }],
  ])('denies a maintainer changing %s', async (_label, change) => {
    await assertFails(updateDoc(packageDoc(owner, RESTRICTED), change));
  });

  it('denies leaving a package without maintainers or with an unknown visibility', async () => {
    await assertFails(updateDoc(packageDoc(owner, RESTRICTED), { maintainers: [] }));
    await assertFails(updateDoc(packageDoc(owner, RESTRICTED), { visibility: 'secret' }));
    await assertFails(updateDoc(packageDoc(owner, RESTRICTED), { readers: 'reader@example.dev' }));
  });

  it('denies readers and strangers any change', async () => {
    await assertFails(updateDoc(packageDoc(reader, RESTRICTED), { readers: ['reader@example.dev', 'x@example.dev'] }));
    await assertFails(updateDoc(packageDoc(stranger, PUBLIC), { maintainers: ['stranger@elsewhere.dev'] }));
  });

  it('denies creating and deleting packages and writing versions from a client', async () => {
    await assertFails(setDoc(packageDoc(owner, 'rules-new'), packageData('rules-new', 'public')));
    await assertFails(deleteDoc(packageDoc(owner, RESTRICTED)));
    await assertFails(setDoc(versionDoc(owner, RESTRICTED), { yanked: true }, { merge: true }));
    await assertFails(
      setDoc(doc(owner.firestore(), 'packages', RESTRICTED, 'versions', '2.0.0'), { version: '2.0.0' }),
    );
  });

  it('keeps the registry configuration server-side', async () => {
    await assertFails(getDoc(doc(owner.firestore(), 'registry', 'config')));
    await assertFails(setDoc(doc(owner.firestore(), 'registry', 'config'), { creators: ['owner@example.dev'] }));
  });
});

describe('artifact storage', () => {
  it('denies uploads, overwrites and listing, even to a maintainer', async () => {
    const storage = owner.storage(`gs://${BUCKET}`);
    await assertFails(uploadBytes(artifact(owner, RESTRICTED), new Uint8Array([9])));
    await assertFails(uploadBytes(ref(storage, `packages/${RESTRICTED}/2.0.0/new.tgz`), new Uint8Array([9])));
    await assertFails(listAll(ref(storage, `packages/${RESTRICTED}`)));
  });

  it('denies reading anything outside the packages tree', async () => {
    await assertFails(getBytes(ref(owner.storage(`gs://${BUCKET}`), 'other/file.txt')));
  });
});
