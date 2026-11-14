// One-off migration for registries deployed before 0.2.0, where the access lists lived on
// the package document itself. Moves them to packages/{name}/private/access and writes
// the grants that go with them. Safe to run more than once.
//
//   cd registry/functions
//   GCLOUD_PROJECT=<project id> node scripts/migrate-access.mjs            # shows what would change
//   GCLOUD_PROJECT=<project id> node scripts/migrate-access.mjs --apply
//
// Credentials: application default credentials, or an OAuth access token in ACCESS_TOKEN
// (for example ACCESS_TOKEN=$(gcloud auth print-access-token)).
import { FieldValue, Firestore } from '@google-cloud/firestore';
import { OAuth2Client } from 'google-auth-library';

const apply = process.argv.includes('--apply');
const projectId = process.env.GCLOUD_PROJECT;
if (!projectId) {
  console.error('Set GCLOUD_PROJECT to the Firebase project id of the registry.');
  process.exit(1);
}

const token = process.env.ACCESS_TOKEN;
const options = { projectId };
if (token) {
  const client = new OAuth2Client();
  client.setCredentials({ access_token: token });
  options.authClient = client;
}
const db = new Firestore(options);

const ACCESS_FIELDS = ['readers', 'readerDomains', 'maintainers', 'trustedPublishers', 'createdBy'];
const list = (value) => (Array.isArray(value) ? value.filter((item) => typeof item === 'string').map((item) => item.toLowerCase()) : []);

const packages = await db.collection('packages').get();
let migrated = 0;
for (const pkg of packages.docs) {
  const data = pkg.data();
  const accessRef = db.doc(`packages/${pkg.id}/private/access`);
  const hasOldFields = ACCESS_FIELDS.some((field) => field in data);
  if (!hasOldFields && (await accessRef.get()).exists) {
    continue;
  }

  const access = {
    visibility: data.visibility === 'public' ? 'public' : 'restricted',
    readers: list(data.readers),
    readerDomains: list(data.readerDomains),
    maintainers: list(data.maintainers),
    trustedPublishers: Array.isArray(data.trustedPublishers) ? data.trustedPublishers : [],
  };
  if (access.maintainers.length === 0) {
    console.error(`${pkg.id}: has no maintainers; fix this package by hand`);
    continue;
  }
  const grants = [
    ...access.readers.map((principal) => ({ package: pkg.id, kind: 'reader', principal })),
    ...access.maintainers.map((principal) => ({ package: pkg.id, kind: 'maintainer', principal })),
    ...access.readerDomains.map((principal) => ({ package: pkg.id, kind: 'domain', principal })),
  ];
  console.log(
    `${pkg.id}: ${access.visibility}, ${access.maintainers.length} maintainer(s), ${access.readers.length} reader(s), ` +
      `${access.readerDomains.length} domain(s), ${access.trustedPublishers.length} trusted publisher(s)`,
  );
  migrated += 1;
  if (!apply) {
    continue;
  }

  const batch = db.batch();
  batch.set(accessRef, { ...access, updatedBy: 'migration', updatedAt: FieldValue.serverTimestamp() });
  for (const grant of grants) {
    batch.set(db.doc(`grants/${grant.package}|${grant.kind}|${grant.principal}`), grant);
  }
  batch.update(pkg.ref, Object.fromEntries(ACCESS_FIELDS.map((field) => [field, FieldValue.delete()])));
  await batch.commit();
}

console.log(apply ? `Migrated ${migrated} package(s).` : `${migrated} package(s) to migrate. Run again with --apply.`);
