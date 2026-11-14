import type { Caller, GithubCaller } from './auth.js';
import { ApiError } from './errors.js';

export type Visibility = 'public' | 'restricted';

/** A CI workflow allowed to publish a package without a personal credential. */
export interface TrustedPublisher {
  provider: 'github';
  /** "owner/repo". */
  repository: string;
  /** Optional: "owner/repo/.github/workflows/file.yml" that must run the job. */
  workflow?: string;
  /** Optional: git ref the job must run on, for example "refs/heads/main". */
  ref?: string;
  /** Optional: deployment environment the job must use. */
  environment?: string;
}

/** Access-related fields of a package document. */
export interface PackageAccess {
  visibility: Visibility;
  readers: string[];
  readerDomains: string[];
  maintainers: string[];
  trustedPublishers: TrustedPublisher[];
}

/** registry/config: who may claim new package names. */
export interface RegistryConfig {
  creators: string[];
  creatorDomains: string[];
}

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
const DOMAIN = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/;
const REPOSITORY = /^[a-z0-9_.-]+\/[a-z0-9_.-]+$/;

function stringList(value: unknown, field: string, max: number): string[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value) || value.some((item) => typeof item !== 'string')) {
    throw new ApiError(400, 'invalid_request', `${field} must be a list of strings`);
  }
  if (value.length > max) {
    throw new ApiError(400, 'invalid_request', `${field} may have at most ${max} entries`);
  }
  return [...new Set((value as string[]).map((item) => item.trim().toLowerCase()))];
}

export function parseEmails(value: unknown, field: string, max: number): string[] {
  const emails = stringList(value, field, max);
  const bad = emails.find((email) => !EMAIL.test(email));
  if (bad !== undefined) {
    throw new ApiError(400, 'invalid_request', `${field}: "${bad}" is not an email address`);
  }
  return emails;
}

export function parseDomains(value: unknown, field: string, max: number): string[] {
  const domains = stringList(value, field, max);
  const bad = domains.find((domain) => !DOMAIN.test(domain));
  if (bad !== undefined) {
    throw new ApiError(400, 'invalid_request', `${field}: "${bad}" is not a domain`);
  }
  return domains;
}

export function parseVisibility(value: unknown): Visibility {
  if (value === undefined) {
    return 'restricted';
  }
  if (value !== 'public' && value !== 'restricted') {
    throw new ApiError(400, 'invalid_request', 'visibility must be "public" or "restricted"');
  }
  return value;
}

export function parseTrustedPublishers(value: unknown): TrustedPublisher[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value) || value.length > 20) {
    throw new ApiError(400, 'invalid_request', 'trustedPublishers must be a list of at most 20 entries');
  }
  return value.map((item, index) => {
    const where = `trustedPublishers[${index}]`;
    if (typeof item !== 'object' || item === null || Array.isArray(item)) {
      throw new ApiError(400, 'invalid_request', `${where} must be an object`);
    }
    const entry = item as Record<string, unknown>;
    if (entry.provider !== 'github') {
      throw new ApiError(400, 'invalid_request', `${where}.provider must be "github"`);
    }
    const repository = typeof entry.repository === 'string' ? entry.repository.trim().toLowerCase() : '';
    if (!REPOSITORY.test(repository)) {
      throw new ApiError(400, 'invalid_request', `${where}.repository must look like "owner/repo"`);
    }
    const publisher: TrustedPublisher = { provider: 'github', repository };
    for (const field of ['workflow', 'ref', 'environment'] as const) {
      const raw = entry[field];
      if (raw === undefined || raw === null || raw === '') {
        continue;
      }
      if (typeof raw !== 'string' || raw.length > 300) {
        throw new ApiError(400, 'invalid_request', `${where}.${field} must be a string`);
      }
      publisher[field] = field === 'workflow' ? raw.trim().toLowerCase() : raw.trim();
    }
    return publisher;
  });
}

export function isMaintainer(caller: Caller, pkg: PackageAccess): boolean {
  return caller.kind === 'user' && pkg.maintainers.includes(caller.email);
}

export function mayCreate(caller: Caller, config: RegistryConfig | undefined): boolean {
  if (caller.kind !== 'user' || config === undefined) {
    return false;
  }
  const domain = caller.email.split('@')[1] ?? '';
  return config.creators.includes(caller.email) || config.creatorDomains.includes(domain);
}

function matchesPublisher(caller: GithubCaller, publisher: TrustedPublisher): boolean {
  return (
    publisher.provider === 'github' &&
    publisher.repository === caller.repository &&
    (publisher.workflow === undefined || publisher.workflow === caller.workflow) &&
    (publisher.ref === undefined || publisher.ref === caller.ref) &&
    (publisher.environment === undefined || publisher.environment === caller.environment)
  );
}

/** Whether the caller may publish a new version of an existing package. */
export function mayPublish(caller: Caller, pkg: PackageAccess): boolean {
  if (caller.kind === 'user') {
    return isMaintainer(caller, pkg);
  }
  return pkg.trustedPublishers.some((publisher) => matchesPublisher(caller, publisher));
}

/** Reads the access fields of a stored package document, tolerating missing lists. */
export function accessOf(data: Record<string, unknown>): PackageAccess {
  const list = (value: unknown): string[] => (Array.isArray(value) ? (value as string[]) : []);
  return {
    visibility: data.visibility === 'public' ? 'public' : 'restricted',
    readers: list(data.readers),
    readerDomains: list(data.readerDomains),
    maintainers: list(data.maintainers),
    trustedPublishers: Array.isArray(data.trustedPublishers) ? (data.trustedPublishers as TrustedPublisher[]) : [],
  };
}

export function configOf(data: Record<string, unknown> | undefined): RegistryConfig | undefined {
  if (data === undefined) {
    return undefined;
  }
  const list = (value: unknown): string[] =>
    Array.isArray(value) ? (value as unknown[]).filter((item): item is string => typeof item === 'string').map((item) => item.toLowerCase()) : [];
  return { creators: list(data.creators), creatorDomains: list(data.creatorDomains) };
}
