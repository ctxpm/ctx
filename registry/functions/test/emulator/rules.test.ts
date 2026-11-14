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

const ACCESS = {
  readers: ['reader@example.dev'],
  readerDomains: ['acme.dev'],
  maintainers: ['owner@example.dev'],
  trustedPublishers: [{ provider: 'github', repository: 'acme/private-repo' }],
};

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
      await setDoc(doc(db, 'packages', name), {
        name,
        type: 'project',
        description: 'rules fixture',
        visibility,
        distTags: { latest: '1.0.0' },
        latestVersion: '1.0.0',
        versionCount: 1,
      });
      await setDoc(doc(db, 'packages', name, 'private', 'access'), { visibility, ...ACCESS });
      await setDoc(doc(db, 'packages', name, 'versions', '1.0.0'), { name, version: '1.0.0', yanked: false });
      await uploadBytes(ref(storage, `packages/${name}/${ARTIFACT}`), new Uint8Array([1, 2, 3]));
      for (const [kind, principal] of [
        ['reader', 'reader@example.dev'],
        ['domain', 'acme.dev'],
        ['maintainer', 'owner@example.dev'],
      ] as const) {
        await setDoc(doc(db, 'grants', `${name}|${kind}|${principal}`), { package: name, kind, principal });
      }
    }
    await setDoc(doc(db, 'registry', 'config'), { creators: ['owner@example.dev'] });
  });
});

afterAll(async () => {
  await env.cleanup();
});

const packageDoc = (context: RulesTestContext, name: string) => doc(context.firestore(), 'packages', name);
const accessDoc = (context: RulesTestContext, name: string) =>
  doc(context.firestore(), 'packages', name, 'private', 'access');
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
    await assertSucceeds(getDocs(collection(who().firestore(), 'packages', RESTRICTED, 'versions')));
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
    await assertFails(getDocs(collection(who().firestore(), 'packages', RESTRICTED, 'versions')));
    await assertFails(getBytes(artifact(who(), RESTRICTED)));
  });

  it('gives the same answer for a package that does not exist', async () => {
    await assertFails(getDoc(packageDoc(reader, 'rules-no-such-package')));
    await assertFails(getDoc(versionDoc(reader, 'rules-no-such-package')));
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

describe('who has access stays between maintainers', () => {
  it('does not put access lists on the document readers receive', async () => {
    const data = (await assertSucceeds(getDoc(packageDoc(reader, RESTRICTED)))).data() ?? {};
    for (const field of ['readers', 'readerDomains', 'maintainers', 'trustedPublishers']) {
      expect(data).not.toHaveProperty(field);
    }
  });

  it('lets a maintainer read the access document', async () => {
    const data = (await assertSucceeds(getDoc(accessDoc(owner, RESTRICTED)))).data();
    expect(data?.readers).toEqual(['reader@example.dev']);
  });

  it.each([
    ['a reader of the package', () => reader, RESTRICTED],
    ['someone from a listed domain', () => colleague, RESTRICTED],
    ['a stranger, even for a public package', () => stranger, PUBLIC],
    ['someone who is not signed in, even for a public package', () => anonymous, PUBLIC],
  ])('denies the access document to %s', async (_label, who, name) => {
    await assertFails(getDoc(accessDoc(who(), name)));
  });

  it('denies listing the private documents of a package', async () => {
    await assertFails(getDocs(collection(owner.firestore(), 'packages', RESTRICTED, 'private')));
  });
});

describe('finding packages', () => {
  const packages = (context: RulesTestContext) => collection(context.firestore(), 'packages');
  const grants = (context: RulesTestContext) => collection(context.firestore(), 'grants');

  it('lets anyone list public packages, and nothing else', async () => {
    const result = await assertSucceeds(getDocs(query(packages(anonymous), where('visibility', '==', 'public'))));
    expect(result.docs.map((entry) => entry.id)).toContain(PUBLIC);
    await assertFails(getDocs(packages(owner)));
    await assertFails(getDocs(packages(anonymous)));
    await assertFails(getDocs(query(packages(owner), where('visibility', '==', 'restricted'))));
  });

  it('lets a reader find what is shared with their address', async () => {
    const result = await assertSucceeds(
      getDocs(query(grants(reader), where('kind', 'in', ['reader', 'maintainer']), where('principal', '==', 'reader@example.dev'))),
    );
    // Other tests share these emulators, so there may be more; every hit is the reader's own.
    expect(result.docs.map((entry) => entry.data().package)).toEqual(expect.arrayContaining([PUBLIC, RESTRICTED]));
    expect(result.docs.every((entry) => entry.data().principal === 'reader@example.dev')).toBe(true);
  });

  it('lets a colleague find what is shared with their domain', async () => {
    const result = await assertSucceeds(
      getDocs(query(grants(colleague), where('kind', '==', 'domain'), where('principal', '==', 'acme.dev'))),
    );
    expect(result.docs.map((entry) => entry.data().package)).toContain(RESTRICTED);
  });

  it('lets a maintainer find the packages they maintain', async () => {
    const result = await assertSucceeds(
      getDocs(query(grants(owner), where('kind', 'in', ['reader', 'maintainer']), where('principal', '==', 'owner@example.dev'))),
    );
    expect(result.docs.map((entry) => entry.data().kind)).toContain('maintainer');
  });

  it("denies looking at someone else's grants", async () => {
    await assertFails(getDocs(grants(reader)));
    await assertFails(getDocs(query(grants(stranger), where('kind', '==', 'reader'), where('principal', '==', 'reader@example.dev'))));
    await assertFails(getDocs(query(grants(stranger), where('kind', '==', 'domain'), where('principal', '==', 'acme.dev'))));
    await assertFails(getDocs(query(grants(reader), where('package', '==', RESTRICTED))));
    await assertFails(getDocs(query(grants(anonymous), where('kind', '==', 'domain'), where('principal', '==', 'acme.dev'))));
    // An address is not a domain grant, and a domain is not an address grant.
    await assertFails(getDocs(query(grants(reader), where('kind', '==', 'domain'), where('principal', '==', 'reader@example.dev'))));
    await assertFails(getDoc(doc(reader.firestore(), 'grants', `${RESTRICTED}|maintainer|owner@example.dev`)));
  });
});

describe('nothing is writable by a client', () => {
  it('denies every change to a package, even to its maintainer', async () => {
    await assertFails(updateDoc(packageDoc(owner, RESTRICTED), { description: 'changed' }));
    await assertFails(updateDoc(packageDoc(owner, RESTRICTED), { visibility: 'public' }));
    await assertFails(setDoc(packageDoc(owner, 'rules-new'), { name: 'rules-new', visibility: 'public' }));
    await assertFails(deleteDoc(packageDoc(owner, RESTRICTED)));
  });

  it('denies changing access, grants and versions', async () => {
    await assertFails(updateDoc(accessDoc(owner, RESTRICTED), { readers: ['reader@example.dev', 'x@example.dev'] }));
    await assertFails(setDoc(accessDoc(stranger, PUBLIC), { visibility: 'public', maintainers: ['stranger@elsewhere.dev'] }));
    await assertFails(
      setDoc(doc(stranger.firestore(), 'grants', `${RESTRICTED}|reader|stranger@elsewhere.dev`), {
        package: RESTRICTED,
        kind: 'reader',
        principal: 'stranger@elsewhere.dev',
      }),
    );
    await assertFails(deleteDoc(doc(reader.firestore(), 'grants', `${RESTRICTED}|reader|reader@example.dev`)));
    await assertFails(setDoc(versionDoc(owner, RESTRICTED), { yanked: true }, { merge: true }));
    await assertFails(setDoc(doc(owner.firestore(), 'packages', RESTRICTED, 'versions', '2.0.0'), { version: '2.0.0' }));
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
