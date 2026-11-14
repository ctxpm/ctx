import { createHash } from 'node:crypto';
import { FieldValue, type Firestore } from 'firebase-admin/firestore';
import type { getStorage } from 'firebase-admin/storage';
import semver from 'semver';
import {
  accessOf,
  configOf,
  isMaintainer,
  mayCreate,
  mayPublish,
  parseDomains,
  parseEmails,
  parseTrustedPublishers,
  parseVisibility,
} from './access.js';
import type { Caller, Identify } from './auth.js';
import { ApiError } from './errors.js';
import { isExactVersion, isPackageName, ManifestError, PACKAGE_TYPE, parseManifest } from './manifest.js';
import { DEFAULT_LIMITS, inspectTarball, TarballError } from './tarball.js';

export interface ApiRequest {
  method: string;
  /** Path below the function, for example "/v1/publish". */
  path: string;
  query: Record<string, unknown>;
  authorization: string | undefined;
  body: Buffer;
}

export interface ApiResponse {
  status: number;
  body: unknown;
}

type Bucket = ReturnType<ReturnType<typeof getStorage>['bucket']>;

export interface Deps {
  db: Firestore;
  bucket: Bucket;
  identify: Identify;
  maxArtifactBytes: number;
  log: (message: string, error: unknown) => void;
}

const DIST_TAG = /^[a-z][a-z0-9-]{0,31}$/;
/**
 * The file list stored on a version document is cut off at this many entries and this many
 * characters of path, so the document stays well under Firestore's 1 MiB limit. The
 * artifact always has every file.
 */
const MAX_LISTED_FILES = 1000;
const MAX_LISTED_PATH_CHARS = 200_000;

function listedFiles(files: { path: string; size: number }[]): { path: string; size: number }[] {
  const listed: { path: string; size: number }[] = [];
  let characters = 0;
  for (const file of files) {
    characters += file.path.length + 32;
    if (listed.length >= MAX_LISTED_FILES || characters > MAX_LISTED_PATH_CHARS) {
      break;
    }
    listed.push(file);
  }
  return listed;
}

/** True when a Storage write failed because the object already exists. */
function alreadyExists(error: unknown): boolean {
  return (error as { code?: number }).code === 412;
}

function publisherOf(caller: Caller): Record<string, unknown> {
  return caller.kind === 'user'
    ? { kind: 'user', email: caller.email }
    : {
        kind: 'github',
        repository: caller.repository,
        workflow: caller.workflow,
        ref: caller.ref,
        sha: caller.sha,
        runId: caller.runId,
      };
}

function parseJson(body: Buffer): Record<string, unknown> {
  let data: unknown;
  try {
    data = JSON.parse(body.toString('utf8'));
  } catch {
    throw new ApiError(400, 'invalid_request', 'Request body must be JSON');
  }
  if (typeof data !== 'object' || data === null || Array.isArray(data)) {
    throw new ApiError(400, 'invalid_request', 'Request body must be a JSON object');
  }
  return data as Record<string, unknown>;
}

async function loadConfig(db: Firestore) {
  const snapshot = await db.doc('registry/config').get();
  return configOf(snapshot.data());
}

/** Fields of a new package document. The creator always becomes a maintainer. */
function newPackage(
  name: string,
  type: string,
  description: string,
  creatorEmail: string,
  access: {
    visibility: 'public' | 'restricted';
    readers: string[];
    readerDomains: string[];
    maintainers: string[];
    trustedPublishers: unknown[];
  },
): Record<string, unknown> {
  return {
    name,
    type,
    description,
    visibility: access.visibility,
    readers: access.readers,
    readerDomains: access.readerDomains,
    maintainers: [...new Set([creatorEmail, ...access.maintainers])],
    trustedPublishers: access.trustedPublishers,
    distTags: {},
    latestVersion: null,
    versionCount: 0,
    createdBy: creatorEmail,
    createdAt: FieldValue.serverTimestamp(),
    updatedAt: FieldValue.serverTimestamp(),
  };
}

/** POST /v1/packages — claim a package name and set its access before the first publish. */
async function createPackage(req: ApiRequest, deps: Deps): Promise<unknown> {
  const caller = await deps.identify(req.authorization);
  const input = parseJson(req.body);

  if (!isPackageName(input.name)) {
    throw new ApiError(400, 'invalid_request', 'name must be lowercase kebab-case, at most 64 characters');
  }
  if (typeof input.type !== 'string' || !PACKAGE_TYPE.test(input.type)) {
    throw new ApiError(400, 'invalid_request', 'type must be a lowercase identifier such as "project"');
  }
  const description = input.description ?? '';
  if (typeof description !== 'string' || description.length > 500) {
    throw new ApiError(400, 'invalid_request', 'description must be a string of at most 500 characters');
  }
  const access = {
    visibility: parseVisibility(input.visibility),
    readers: parseEmails(input.readers, 'readers', 500),
    readerDomains: parseDomains(input.readerDomains, 'readerDomains', 100),
    maintainers: parseEmails(input.maintainers, 'maintainers', 99),
    trustedPublishers: parseTrustedPublishers(input.trustedPublishers),
  };

  if (caller.kind !== 'user' || !mayCreate(caller, await loadConfig(deps.db))) {
    throw new ApiError(403, 'forbidden', 'You are not allowed to create packages in this registry');
  }

  const ref = deps.db.collection('packages').doc(input.name);
  try {
    await ref.create(newPackage(input.name, input.type, description, caller.email, access));
  } catch (error) {
    if ((error as { code?: number }).code === 6) {
      throw new ApiError(409, 'package_exists', `Package "${input.name}" already exists`);
    }
    throw error;
  }
  return { name: input.name };
}

/** POST /v1/publish — body is the .tgz; name and version come from its manifest.json. */
async function publish(req: ApiRequest, deps: Deps): Promise<unknown> {
  const caller = await deps.identify(req.authorization);

  if (req.body.length === 0) {
    throw new ApiError(400, 'invalid_request', 'Request body must be the package archive');
  }
  if (req.body.length > deps.maxArtifactBytes) {
    throw new ApiError(413, 'artifact_too_large', `Archive is larger than ${deps.maxArtifactBytes} bytes`);
  }

  const rawTag = req.query.tag;
  if (rawTag !== undefined && (typeof rawTag !== 'string' || !DIST_TAG.test(rawTag))) {
    throw new ApiError(400, 'invalid_request', 'tag must be a lowercase identifier such as "next"');
  }
  const tag = rawTag as string | undefined;

  let info;
  let manifest;
  try {
    info = await inspectTarball(req.body, DEFAULT_LIMITS);
    manifest = parseManifest(info.manifestRaw);
  } catch (error) {
    if (error instanceof ManifestError) {
      throw new ApiError(422, 'invalid_manifest', error.message, error.problems);
    }
    if (error instanceof TarballError) {
      throw new ApiError(422, 'invalid_archive', error.message);
    }
    throw error;
  }

  const { db, bucket } = deps;
  const pkgRef = db.collection('packages').doc(manifest.name);
  const versionRef = pkgRef.collection('versions').doc(manifest.version);
  const config = await loadConfig(db);

  /** Throws unless the caller may publish this manifest given the current package document. */
  const authorize = (pkg: Record<string, unknown> | undefined): void => {
    if (pkg === undefined) {
      if (!mayCreate(caller, config)) {
        throw new ApiError(
          403,
          'forbidden',
          `Package "${manifest.name}" does not exist and you are not allowed to create packages in this registry`,
        );
      }
      return;
    }
    if (!mayPublish(caller, accessOf(pkg))) {
      // Do not reveal whether a package exists to someone who cannot publish to it.
      throw new ApiError(403, 'forbidden', `You are not allowed to publish "${manifest.name}"`);
    }
    if (pkg.type !== manifest.type) {
      throw new ApiError(
        422,
        'type_mismatch',
        `Package "${manifest.name}" has type "${String(pkg.type)}"; this archive declares "${manifest.type}"`,
      );
    }
  };

  const versionExists = (): ApiError =>
    new ApiError(409, 'version_exists', `${manifest.name}@${manifest.version} is already published; versions are immutable`);

  // Check before storing anything, then again inside the transaction.
  authorize((await pkgRef.get()).data());
  if ((await versionRef.get()).exists) {
    throw versionExists();
  }

  const sha256 = createHash('sha256').update(req.body).digest('hex');
  // The object name contains the digest, so different bytes can never replace a published
  // artifact, and the write only succeeds when the object does not exist yet.
  const artifact = `packages/${manifest.name}/${manifest.version}/${sha256}.tgz`;
  let uploaded = true;
  try {
    await bucket.file(artifact).save(req.body, {
      resumable: false,
      contentType: 'application/gzip',
      metadata: { metadata: { sha256, package: manifest.name, version: manifest.version } },
      preconditionOpts: { ifGenerationMatch: 0 },
    });
  } catch (error) {
    if (!alreadyExists(error)) {
      throw error;
    }
    // Same name means same bytes: another request stored this exact archive already.
    uploaded = false;
  }

  try {
    return await db.runTransaction(async (tx) => {
      const pkgSnapshot = await tx.get(pkgRef);
      const versionSnapshot = await tx.get(versionRef);
      const pkg = pkgSnapshot.data();
      authorize(pkg);
      if (versionSnapshot.exists) {
        throw versionExists();
      }

      const distTags: Record<string, string> = { ...((pkg?.distTags as Record<string, string> | undefined) ?? {}) };
      const isPrerelease = semver.prerelease(manifest.version) !== null;
      if (tag !== undefined) {
        distTags[tag] = manifest.version;
      } else if (!isPrerelease && (distTags.latest === undefined || semver.gt(manifest.version, distTags.latest))) {
        distTags.latest = manifest.version;
      }
      const previousHighest = pkg?.latestVersion as string | null | undefined;
      const latestVersion =
        previousHighest && semver.gt(previousHighest, manifest.version) ? previousHighest : manifest.version;

      if (pkg === undefined) {
        // authorize() only lets a signed-in user get here.
        const email = caller.kind === 'user' ? caller.email : '';
        tx.create(pkgRef, {
          ...newPackage(manifest.name, manifest.type, manifest.description, email, {
            visibility: 'restricted',
            readers: [],
            readerDomains: [],
            maintainers: [],
            trustedPublishers: [],
          }),
          distTags,
          latestVersion,
          versionCount: 1,
        });
      } else {
        tx.update(pkgRef, {
          distTags,
          latestVersion,
          versionCount: FieldValue.increment(1),
          updatedAt: FieldValue.serverTimestamp(),
          // The description shown in listings follows the newest stable release.
          ...(distTags.latest === manifest.version ? { description: manifest.description } : {}),
        });
      }

      tx.create(versionRef, {
        name: manifest.name,
        version: manifest.version,
        type: manifest.type,
        description: manifest.description,
        dependencies: manifest.dependencies,
        manifestJson: info.manifestRaw,
        integrity: `sha256-${sha256}`,
        artifact,
        archivePrefix: info.prefix,
        size: req.body.length,
        unpackedSize: info.unpackedSize,
        fileCount: info.files.length,
        files: listedFiles(info.files),
        readme: info.readme,
        yanked: false,
        publishedBy: publisherOf(caller),
        publishedAt: FieldValue.serverTimestamp(),
      });

      return {
        name: manifest.name,
        version: manifest.version,
        integrity: `sha256-${sha256}`,
        distTags,
      };
    });
  } catch (error) {
    // Clean up only in the one case that is certain: this request stored the archive, lost
    // to a version that is published with a different archive, and that could be read
    // back. In every other case the object stays; an unreferenced archive is harmless,
    // a deleted published one is not.
    if (uploaded && error instanceof ApiError && error.code === 'version_exists') {
      try {
        const published = await versionRef.get();
        const publishedArtifact = published.data()?.artifact;
        if (published.exists && typeof publishedArtifact === 'string' && publishedArtifact !== artifact) {
          await bucket.file(artifact).delete({ ignoreNotFound: true });
        }
      } catch (cleanupError) {
        deps.log(`Left ${artifact} in place; could not confirm it is unused`, cleanupError);
      }
    }
    throw error;
  }
}

/** POST /v1/packages/{name}/versions/{version}/yank — body {"yanked": true|false}. */
async function yank(req: ApiRequest, deps: Deps, name: string, version: string): Promise<unknown> {
  const caller = await deps.identify(req.authorization);
  const input = parseJson(req.body);
  if (typeof input.yanked !== 'boolean') {
    throw new ApiError(400, 'invalid_request', 'yanked must be true or false');
  }
  if (!isPackageName(name) || !isExactVersion(version)) {
    throw new ApiError(404, 'not_found', 'No such version');
  }
  const yanked = input.yanked;
  const pkgRef = deps.db.collection('packages').doc(name);
  const versionRef = pkgRef.collection('versions').doc(version);

  return deps.db.runTransaction(async (tx) => {
    const pkgSnapshot = await tx.get(pkgRef);
    const pkg = pkgSnapshot.data();
    if (pkg === undefined || !isMaintainer(caller, accessOf(pkg))) {
      throw new ApiError(403, 'forbidden', `You are not allowed to change "${name}"`);
    }
    const versions = await tx.get(pkgRef.collection('versions').select('yanked'));
    if (!versions.docs.some((doc) => doc.id === version)) {
      throw new ApiError(404, 'not_found', 'No such version');
    }

    // "latest" always points at the highest stable version that is not yanked.
    const available = versions.docs
      .filter((doc) => (doc.id === version ? !yanked : doc.get('yanked') !== true))
      .map((doc) => doc.id)
      .filter((id) => semver.valid(id) !== null && semver.prerelease(id) === null)
      .sort(semver.rcompare);
    const distTags: Record<string, string> = { ...((pkg.distTags as Record<string, string> | undefined) ?? {}) };
    if (available[0] === undefined) {
      delete distTags.latest;
    } else {
      distTags.latest = available[0];
    }

    tx.update(versionRef, { yanked });
    tx.update(pkgRef, { distTags, updatedAt: FieldValue.serverTimestamp() });
    return { name, version, yanked, distTags };
  });
}

/** Routes one request. Never throws: every failure becomes a JSON error response. */
export async function handle(req: ApiRequest, deps: Deps): Promise<ApiResponse> {
  try {
    let segments: string[];
    try {
      segments = req.path.split('/').filter(Boolean).map(decodeURIComponent);
    } catch {
      throw new ApiError(400, 'invalid_request', 'Malformed path');
    }
    const [versionSegment, ...route] = segments;
    if (versionSegment !== 'v1') {
      throw new ApiError(404, 'not_found', 'Unknown route');
    }

    if (req.method === 'GET' && route.length === 1 && route[0] === 'health') {
      return { status: 200, body: { ok: true } };
    }
    if (req.method === 'POST' && route.length === 1 && route[0] === 'publish') {
      return { status: 201, body: await publish(req, deps) };
    }
    if (req.method === 'POST' && route.length === 1 && route[0] === 'packages') {
      return { status: 201, body: await createPackage(req, deps) };
    }
    if (
      req.method === 'POST' &&
      route.length === 5 &&
      route[0] === 'packages' &&
      route[2] === 'versions' &&
      route[4] === 'yank'
    ) {
      return { status: 200, body: await yank(req, deps, route[1] ?? '', route[3] ?? '') };
    }
    throw new ApiError(404, 'not_found', 'Unknown route');
  } catch (error) {
    if (error instanceof ApiError) {
      return {
        status: error.status,
        body: { error: { code: error.code, message: error.message, ...(error.details ? { details: error.details } : {}) } },
      };
    }
    deps.log('Unhandled error', error);
    return { status: 500, body: { error: { code: 'internal', message: 'Internal error' } } };
  }
}
