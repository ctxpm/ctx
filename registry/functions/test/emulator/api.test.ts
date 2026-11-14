import { createHash } from 'node:crypto';
import { createServer, type Server } from 'node:http';
import { exportJWK, generateKeyPair, SignJWT, type JWK } from 'jose';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { packageTgz, tgz } from '../tarballs.js';
import { adminBucket, adminDb, API, BUCKET, seedPackage, signIn, STORAGE, uniqueName } from './helpers.js';

// Must match registry/functions/.env.demo-ctx.
const ISSUER = 'http://127.0.0.1:9911';
const AUDIENCE = 'ctx:demo-ctx';

type PrivateKey = Awaited<ReturnType<typeof generateKeyPair>>['privateKey'];

let keyServer: Server;
let ciKey: PrivateKey;
let unknownKey: PrivateKey;
let owner: string;
let reader: string;
let stranger: string;

interface ApiResult {
  status: number;
  body: Record<string, any>;
}

async function call(path: string, init: { token?: string; body?: Buffer | string; method?: string } = {}): Promise<ApiResult> {
  const response = await fetch(`${API}${path}`, {
    method: init.method ?? 'POST',
    headers: {
      ...(init.token ? { authorization: `Bearer ${init.token}` } : {}),
      'content-type': typeof init.body === 'string' ? 'application/json' : 'application/gzip',
    },
    body: init.body === undefined ? undefined : typeof init.body === 'string' ? init.body : new Uint8Array(init.body),
  });
  // Malformed JSON is rejected by the platform before the handler runs, with a non-JSON body.
  const text = await response.text();
  let body: Record<string, any> = {};
  try {
    body = JSON.parse(text) as Record<string, any>;
  } catch {
    body = { raw: text };
  }
  return { status: response.status, body };
}

const publish = (token: string | undefined, archive: Buffer, query = '') =>
  call(`/publish${query}`, { token, body: archive });

/** A token as GitHub Actions would issue it, signed by the test key server. */
async function ciToken(
  claims: Record<string, unknown> = {},
  options: { key?: PrivateKey; audience?: string } = {},
): Promise<string> {
  return new SignJWT({
    repository: 'acme/handbook',
    job_workflow_ref: 'acme/handbook/.github/workflows/release.yml@refs/heads/main',
    ref: 'refs/heads/main',
    sha: '0123456789abcdef',
    run_id: '42',
    ...claims,
  })
    .setProtectedHeader({ alg: 'RS256', kid: 'test-key' })
    .setIssuer(ISSUER)
    .setAudience(options.audience ?? AUDIENCE)
    .setIssuedAt()
    .setExpirationTime('5m')
    .sign(options.key ?? ciKey);
}

/** Downloads an artifact the way a client does: Storage REST API with the user's ID token. */
async function download(artifact: string, token?: string): Promise<{ status: number; bytes: Buffer }> {
  const response = await fetch(
    `http://${STORAGE.host}:${STORAGE.port}/v0/b/${BUCKET}/o/${encodeURIComponent(artifact)}?alt=media`,
    { headers: token ? { authorization: `Firebase ${token}` } : {} },
  );
  return { status: response.status, bytes: Buffer.from(await response.arrayBuffer()) };
}

beforeAll(async () => {
  const pair = await generateKeyPair('RS256');
  ciKey = pair.privateKey;
  unknownKey = (await generateKeyPair('RS256')).privateKey;
  const jwk: JWK = { ...(await exportJWK(pair.publicKey)), kid: 'test-key', alg: 'RS256', use: 'sig' };
  keyServer = createServer((_request, response) => {
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ keys: [jwk] }));
  });
  await new Promise<void>((resolve) => keyServer.listen(9911, '127.0.0.1', resolve));

  await adminDb.doc('registry/config').set({ creators: ['owner@example.dev'], creatorDomains: ['creators.dev'] });
  [owner, reader, stranger] = await Promise.all([
    signIn('owner@example.dev'),
    signIn('reader@example.dev'),
    signIn('stranger@elsewhere.dev'),
  ]);
});

afterAll(async () => {
  await new Promise((resolve) => keyServer.close(resolve));
});

describe('routing and authentication', () => {
  it('answers the health check', async () => {
    expect(await call('/health', { method: 'GET' })).toEqual({ status: 200, body: { ok: true } });
  });

  it('returns 404 for unknown routes', async () => {
    expect((await call('/nope', { token: owner })).status).toBe(404);
  });

  it('rejects a request without a token', async () => {
    const result = await publish(undefined, await packageTgz(uniqueName('noauth'), '1.0.0'));
    expect(result.status).toBe(401);
  });

  it('rejects a token that is not a JWT', async () => {
    expect((await publish('garbage', await packageTgz(uniqueName('noauth'), '1.0.0'))).status).toBe(401);
  });

  it('rejects a user whose email is not verified', async () => {
    const unverified = await signIn('owner-unverified@example.dev', false);
    const result = await publish(unverified, await packageTgz(uniqueName('unverified'), '1.0.0'));
    expect(result.status).toBe(403);
    expect(result.body.error.code).toBe('email_not_verified');
  });
});

describe('publishing as a user', () => {
  it('creates the package on first publish and stores the version and artifact', async () => {
    const name = uniqueName('first');
    const archive = await packageTgz(name, '1.0.0', { extra: { dependencies: { base: '^1.0.0' } } });
    const sha256 = createHash('sha256').update(archive).digest('hex');

    const result = await publish(owner, archive);
    expect(result.status).toBe(201);
    expect(result.body).toEqual({
      name,
      version: '1.0.0',
      integrity: `sha256-${sha256}`,
      distTags: { latest: '1.0.0' },
    });

    const pkg = (await adminDb.doc(`packages/${name}`).get()).data();
    expect(pkg).toMatchObject({
      name,
      type: 'project',
      visibility: 'restricted',
      readers: [],
      maintainers: ['owner@example.dev'],
      distTags: { latest: '1.0.0' },
      latestVersion: '1.0.0',
      versionCount: 1,
    });

    const version = (await adminDb.doc(`packages/${name}/versions/1.0.0`).get()).data();
    expect(version).toMatchObject({
      name,
      version: '1.0.0',
      dependencies: { base: '^1.0.0' },
      integrity: `sha256-${sha256}`,
      artifact: `packages/${name}/1.0.0/${sha256}.tgz`,
      archivePrefix: '',
      size: archive.length,
      fileCount: 3,
      readme: `# ${name}\n`,
      yanked: false,
      publishedBy: { kind: 'user', email: 'owner@example.dev' },
    });
    expect(JSON.parse(version?.manifestJson).name).toBe(name);

    const [stored] = await adminBucket.file(`packages/${name}/1.0.0/${sha256}.tgz`).download();
    expect(stored.equals(archive)).toBe(true);
  });

  it('lets someone from a creator domain create a package', async () => {
    const token = await signIn('dev@creators.dev');
    expect((await publish(token, await packageTgz(uniqueName('domain'), '1.0.0'))).status).toBe(201);
  });

  it('denies creating a package to someone who is not a creator, and stores nothing', async () => {
    const name = uniqueName('denied');
    const result = await publish(stranger, await packageTgz(name, '1.0.0'));
    expect(result.status).toBe(403);
    expect((await adminDb.doc(`packages/${name}`).get()).exists).toBe(false);
    const [files] = await adminBucket.getFiles({ prefix: `packages/${name}/` });
    expect(files).toHaveLength(0);
  });

  it('denies publishing to an existing package to a reader who is not a maintainer', async () => {
    const name = uniqueName('readonly');
    await seedPackage(name, { readers: ['reader@example.dev'] });
    const result = await publish(reader, await packageTgz(name, '1.0.0'));
    expect(result.status).toBe(403);
    const [files] = await adminBucket.getFiles({ prefix: `packages/${name}/` });
    expect(files).toHaveLength(0);
  });

  it('never replaces a published version, with the same or with different content', async () => {
    const name = uniqueName('immutable');
    const original = await packageTgz(name, '1.0.0');
    const originalSha = createHash('sha256').update(original).digest('hex');
    expect((await publish(owner, original)).status).toBe(201);

    const same = await publish(owner, original);
    expect(same.status).toBe(409);
    expect(same.body.error.code).toBe('version_exists');

    const different = await publish(owner, await packageTgz(name, '1.0.0', { files: { 'extra.md': 'changed' } }));
    expect(different.status).toBe(409);

    const [files] = await adminBucket.getFiles({ prefix: `packages/${name}/1.0.0/` });
    expect(files.map((file) => file.name)).toEqual([`packages/${name}/1.0.0/${originalSha}.tgz`]);
    const [stored] = await files[0]!.download();
    expect(stored.equals(original)).toBe(true);
    const version = (await adminDb.doc(`packages/${name}/versions/1.0.0`).get()).data();
    expect(version?.integrity).toBe(`sha256-${originalSha}`);
  });

  it('accepts only one of several simultaneous publishes of the same version', async () => {
    const name = uniqueName('race');
    expect((await publish(owner, await packageTgz(name, '1.0.0'))).status).toBe(201);
    const attempts = await Promise.all(
      ['a', 'b', 'c', 'd'].map(async (variant) =>
        publish(owner, await packageTgz(name, '1.1.0', { files: { 'variant.md': variant } })),
      ),
    );
    expect(attempts.map((attempt) => attempt.status).sort()).toEqual([201, 409, 409, 409]);
    const [files] = await adminBucket.getFiles({ prefix: `packages/${name}/1.1.0/` });
    expect(files).toHaveLength(1);
    const version = (await adminDb.doc(`packages/${name}/versions/1.1.0`).get()).data();
    expect(files[0]!.name).toBe(version?.artifact);
    expect((await adminDb.doc(`packages/${name}`).get()).data()?.versionCount).toBe(2);
  });

  it('rejects a version whose type differs from the package type', async () => {
    const name = uniqueName('typed');
    expect((await publish(owner, await packageTgz(name, '1.0.0'))).status).toBe(201);
    const result = await publish(owner, await packageTgz(name, '1.1.0', { extra: { type: 'discipline' } }));
    expect(result.status).toBe(422);
    expect(result.body.error.code).toBe('type_mismatch');
  });

  it('rejects invalid archives and manifests with a reason', async () => {
    const notAnArchive = await publish(owner, Buffer.from('hello'));
    expect(notAnArchive.status).toBe(422);
    expect(notAnArchive.body.error.code).toBe('invalid_archive');

    const badManifest = await publish(
      owner,
      await tgz([{ name: 'manifest.json', content: JSON.stringify({ schemaVersion: 1, name: 'Bad Name', version: '1' }) }]),
    );
    expect(badManifest.status).toBe(422);
    expect(badManifest.body.error.code).toBe('invalid_manifest');
    expect(badManifest.body.error.details).toHaveLength(3);

    const symlink = await publish(
      owner,
      await tgz([
        { name: 'manifest.json', content: JSON.stringify({ schemaVersion: 1, name: uniqueName('sym'), version: '1.0.0', type: 'project' }) },
        { name: 'link', type: 'symlink', linkname: '/etc/passwd' },
      ]),
    );
    expect(symlink.status).toBe(422);

    expect((await publish(owner, Buffer.alloc(0))).status).toBe(400);
  });

  it('accepts an archive with one top-level directory and records the prefix', async () => {
    const name = uniqueName('prefixed');
    expect((await publish(owner, await packageTgz(name, '1.0.0', { prefix: 'package/' }))).status).toBe(201);
    const version = (await adminDb.doc(`packages/${name}/versions/1.0.0`).get()).data();
    expect(version?.archivePrefix).toBe('package/');
    expect(version?.files.map((file: { path: string }) => file.path)).toContain('manifest.json');
  });
});

describe('dist-tags and yanking', () => {
  it('keeps "latest" on the highest stable version and supports named tags', async () => {
    const name = uniqueName('tags');
    const tagsAfter = async (version: string, query = '') =>
      (await publish(owner, await packageTgz(name, version), query)).body.distTags;

    expect(await tagsAfter('1.0.0')).toEqual({ latest: '1.0.0' });
    expect(await tagsAfter('1.1.0')).toEqual({ latest: '1.1.0' });
    expect(await tagsAfter('1.0.5')).toEqual({ latest: '1.1.0' });
    expect(await tagsAfter('2.0.0-rc.1')).toEqual({ latest: '1.1.0' });
    expect(await tagsAfter('2.0.0-rc.2', '?tag=next')).toEqual({ latest: '1.1.0', next: '2.0.0-rc.2' });

    const pkg = (await adminDb.doc(`packages/${name}`).get()).data();
    expect(pkg?.latestVersion).toBe('2.0.0-rc.2');
    expect(pkg?.versionCount).toBe(5);

    expect((await publish(owner, await packageTgz(name, '3.0.0'), '?tag=Not%20Valid')).status).toBe(400);
  });

  it('moves "latest" away from a yanked version and back when it is restored', async () => {
    const name = uniqueName('yank');
    await publish(owner, await packageTgz(name, '1.0.0'));
    await publish(owner, await packageTgz(name, '1.1.0'));

    const yanked = await call(`/packages/${name}/versions/1.1.0/yank`, { token: owner, body: '{"yanked":true}' });
    expect(yanked).toEqual({ status: 200, body: { name, version: '1.1.0', yanked: true, distTags: { latest: '1.0.0' } } });
    expect((await adminDb.doc(`packages/${name}/versions/1.1.0`).get()).data()?.yanked).toBe(true);

    const restored = await call(`/packages/${name}/versions/1.1.0/yank`, { token: owner, body: '{"yanked":false}' });
    expect(restored.body.distTags).toEqual({ latest: '1.1.0' });

    await call(`/packages/${name}/versions/1.0.0/yank`, { token: owner, body: '{"yanked":true}' });
    const all = await call(`/packages/${name}/versions/1.1.0/yank`, { token: owner, body: '{"yanked":true}' });
    expect(all.body.distTags).toEqual({});
  });

  it('denies yanking to non-maintainers and reports unknown versions', async () => {
    const name = uniqueName('yank-denied');
    await publish(owner, await packageTgz(name, '1.0.0'));
    expect((await call(`/packages/${name}/versions/1.0.0/yank`, { token: stranger, body: '{"yanked":true}' })).status).toBe(403);
    expect((await call(`/packages/${name}/versions/9.9.9/yank`, { token: owner, body: '{"yanked":true}' })).status).toBe(404);
    expect((await call(`/packages/${name}/versions/1.0.0/yank`, { token: owner, body: '{"yanked":"yes"}' })).status).toBe(400);
  });
});

describe('creating a package up front', () => {
  it('stores the access settings and makes the creator a maintainer', async () => {
    const name = uniqueName('created');
    const result = await call('/packages', {
      token: owner,
      body: JSON.stringify({
        name,
        type: 'project',
        description: 'Created before the first publish',
        visibility: 'public',
        readers: ['Reader@Example.dev'],
        readerDomains: ['Acme.dev'],
        maintainers: ['second@example.dev'],
        trustedPublishers: [{ provider: 'github', repository: 'Acme/Handbook' }],
      }),
    });
    expect(result).toEqual({ status: 201, body: { name } });
    expect((await adminDb.doc(`packages/${name}`).get()).data()).toMatchObject({
      visibility: 'public',
      readers: ['reader@example.dev'],
      readerDomains: ['acme.dev'],
      maintainers: ['owner@example.dev', 'second@example.dev'],
      trustedPublishers: [{ provider: 'github', repository: 'acme/handbook' }],
      versionCount: 0,
    });

    const again = await call('/packages', { token: owner, body: JSON.stringify({ name, type: 'project' }) });
    expect(again.status).toBe(409);
  });

  it('denies non-creators and rejects invalid input', async () => {
    const body = JSON.stringify({ name: uniqueName('nope'), type: 'project' });
    expect((await call('/packages', { token: stranger, body })).status).toBe(403);
    expect((await call('/packages', { token: owner, body: JSON.stringify({ name: 'Bad Name', type: 'project' }) })).status).toBe(400);
    expect((await call('/packages', { token: owner, body: JSON.stringify({ name: uniqueName('x'), type: 'project', readers: ['nope'] }) })).status).toBe(400);
    expect((await call('/packages', { token: owner, body: 'not json' })).status).toBe(400);
  });
});

describe('publishing from CI', () => {
  async function trustedPackage(publisher: Record<string, unknown> = {}): Promise<string> {
    const name = uniqueName('ci');
    await seedPackage(name, { trustedPublishers: [{ provider: 'github', repository: 'acme/handbook', ...publisher }] });
    return name;
  }

  it('accepts a job from a trusted repository and records where it came from', async () => {
    const name = await trustedPackage();
    const result = await publish(await ciToken(), await packageTgz(name, '1.0.0'));
    expect(result.status).toBe(201);
    const version = (await adminDb.doc(`packages/${name}/versions/1.0.0`).get()).data();
    expect(version?.publishedBy).toEqual({
      kind: 'github',
      repository: 'acme/handbook',
      workflow: 'acme/handbook/.github/workflows/release.yml',
      ref: 'refs/heads/main',
      sha: '0123456789abcdef',
      runId: '42',
    });
  });

  it('matches the repository without regard to case', async () => {
    const name = await trustedPackage();
    expect((await publish(await ciToken({ repository: 'Acme/Handbook' }), await packageTgz(name, '1.0.0'))).status).toBe(201);
  });

  it('denies a job from another repository', async () => {
    const name = await trustedPackage();
    const result = await publish(await ciToken({ repository: 'mallory/handbook' }), await packageTgz(name, '1.0.0'));
    expect(result.status).toBe(403);
    expect((await adminDb.doc(`packages/${name}/versions/1.0.0`).get()).exists).toBe(false);
  });

  it('enforces a pinned workflow and ref', async () => {
    const name = await trustedPackage({
      workflow: 'acme/handbook/.github/workflows/release.yml',
      ref: 'refs/heads/main',
    });
    const fromBranch = await ciToken({ ref: 'refs/heads/feature' });
    const otherWorkflow = await ciToken({ job_workflow_ref: 'acme/handbook/.github/workflows/ci.yml@refs/heads/main' });
    expect((await publish(fromBranch, await packageTgz(name, '1.0.0'))).status).toBe(403);
    expect((await publish(otherWorkflow, await packageTgz(name, '1.0.0'))).status).toBe(403);
    expect((await publish(await ciToken(), await packageTgz(name, '1.0.0'))).status).toBe(201);
  });

  it('rejects a token for another audience, signed by an unknown key, or expired', async () => {
    const name = await trustedPackage();
    const archive = await packageTgz(name, '1.0.0');
    expect((await publish(await ciToken({}, { audience: 'someone-else' }), archive)).status).toBe(401);
    expect((await publish(await ciToken({}, { key: unknownKey }), archive)).status).toBe(401);
    const expired = await new SignJWT({ repository: 'acme/handbook' })
      .setProtectedHeader({ alg: 'RS256', kid: 'test-key' })
      .setIssuer(ISSUER)
      .setAudience(AUDIENCE)
      .setIssuedAt(Math.floor(Date.now() / 1000) - 7200)
      .setExpirationTime(Math.floor(Date.now() / 1000) - 3600)
      .sign(ciKey);
    expect((await publish(expired, archive)).status).toBe(401);
    expect((await adminDb.doc(`packages/${name}/versions/1.0.0`).get()).exists).toBe(false);
  });

  it('never lets a CI job create a package or yank a version', async () => {
    const result = await publish(await ciToken(), await packageTgz(uniqueName('ci-new'), '1.0.0'));
    expect(result.status).toBe(403);

    const name = await trustedPackage();
    await publish(await ciToken(), await packageTgz(name, '1.0.0'));
    const yank = await call(`/packages/${name}/versions/1.0.0/yank`, { token: await ciToken(), body: '{"yanked":true}' });
    expect(yank.status).toBe(403);
  });
});

describe('downloading what was published', () => {
  it('serves the artifact to entitled readers only, byte for byte', async () => {
    const name = uniqueName('download');
    const archive = await packageTgz(name, '1.0.0');
    const published = await publish(owner, archive);
    const artifact = (await adminDb.doc(`packages/${name}/versions/1.0.0`).get()).data()?.artifact as string;

    expect((await download(artifact, reader)).status).toBe(403);
    expect((await download(artifact)).status).toBe(403);

    await adminDb.doc(`packages/${name}`).update({ readers: ['reader@example.dev'] });

    const asReader = await download(artifact, reader);
    expect(asReader.status).toBe(200);
    expect(`sha256-${createHash('sha256').update(asReader.bytes).digest('hex')}`).toBe(published.body.integrity);
    expect((await download(artifact, owner)).status).toBe(200);
    expect((await download(artifact, stranger)).status).toBe(403);
    expect((await download(artifact)).status).toBe(403);
  });
});
