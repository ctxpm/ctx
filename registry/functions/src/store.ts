// Where access lives in Firestore.
//
//   packages/{name}                  what readers see: type, description, tags, counts
//   packages/{name}/private/access   who may read, maintain and publish; maintainers only
//   grants/{package}|{kind}|{who}    one entry per reader, domain or maintainer, readable
//                                    only by whom it names: how a client finds what is
//                                    shared with it
//
// The access document is the source of truth; grants are derived from it and written in
// the same transaction. Nothing here is writable by clients.

import { FieldValue, type DocumentReference, type Firestore, type Transaction } from 'firebase-admin/firestore';
import type { PackageAccess } from './access.js';

export type GrantKind = 'reader' | 'maintainer' | 'domain';

export interface Grant {
  package: string;
  kind: GrantKind;
  principal: string;
}

export function accessRef(db: Firestore, name: string): DocumentReference {
  return db.doc(`packages/${name}/private/access`);
}

function grantRef(db: Firestore, grant: Grant): DocumentReference {
  return db.doc(`grants/${grant.package}|${grant.kind}|${grant.principal}`);
}

export function grantsOf(name: string, access: PackageAccess): Grant[] {
  return [
    ...access.readers.map((principal) => ({ package: name, kind: 'reader' as const, principal })),
    ...access.maintainers.map((principal) => ({ package: name, kind: 'maintainer' as const, principal })),
    ...access.readerDomains.map((principal) => ({ package: name, kind: 'domain' as const, principal })),
  ];
}

const keyOf = (grant: Grant): string => `${grant.kind}|${grant.principal}`;

/**
 * Writes the access document of a package and brings its grants in line with it.
 * `previous` is the access the package had, or undefined for a new package.
 */
export function writeAccess(
  tx: Transaction,
  db: Firestore,
  name: string,
  previous: PackageAccess | undefined,
  next: PackageAccess,
  changedBy: string,
): void {
  tx.set(accessRef(db, name), {
    // Repeated here so that one lookup tells the security rules everything they need.
    visibility: next.visibility,
    readers: next.readers,
    readerDomains: next.readerDomains,
    maintainers: next.maintainers,
    trustedPublishers: next.trustedPublishers,
    updatedBy: changedBy,
    updatedAt: FieldValue.serverTimestamp(),
  });

  const before = new Map((previous ? grantsOf(name, previous) : []).map((grant) => [keyOf(grant), grant]));
  const after = new Map(grantsOf(name, next).map((grant) => [keyOf(grant), grant]));
  for (const [key, grant] of before) {
    if (!after.has(key)) {
      tx.delete(grantRef(db, grant));
    }
  }
  for (const [key, grant] of after) {
    if (!before.has(key)) {
      tx.set(grantRef(db, grant), grant);
    }
  }
}
