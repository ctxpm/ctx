import { describe, expect, it } from 'vitest';
import {
  mayCreate,
  mayPublish,
  parseDomains,
  parseEmails,
  parseTrustedPublishers,
  type PackageAccess,
} from '../../src/access.js';
import type { GithubCaller, UserCaller } from '../../src/auth.js';

const user = (email: string): UserCaller => ({ kind: 'user', uid: email, email });

const ci = (overrides: Partial<GithubCaller> = {}): GithubCaller => ({
  kind: 'github',
  repository: 'acme/handbook',
  workflow: 'acme/handbook/.github/workflows/release.yml',
  ref: 'refs/heads/main',
  environment: null,
  sha: 'abc',
  runId: '1',
  ...overrides,
});

const pkg = (overrides: Partial<PackageAccess> = {}): PackageAccess => ({
  visibility: 'restricted',
  readers: ['reader@example.dev'],
  readerDomains: [],
  maintainers: ['owner@example.dev'],
  trustedPublishers: [],
  ...overrides,
});

describe('mayPublish', () => {
  it('allows maintainers and nobody else', () => {
    expect(mayPublish(user('owner@example.dev'), pkg())).toBe(true);
    expect(mayPublish(user('reader@example.dev'), pkg())).toBe(false);
  });

  it('allows a CI job only from a trusted repository', () => {
    const trusted = pkg({ trustedPublishers: [{ provider: 'github', repository: 'acme/handbook' }] });
    expect(mayPublish(ci(), trusted)).toBe(true);
    expect(mayPublish(ci({ repository: 'mallory/handbook' }), trusted)).toBe(false);
    expect(mayPublish(ci(), pkg())).toBe(false);
  });

  it('enforces workflow, ref and environment when they are pinned', () => {
    const pinned = pkg({
      trustedPublishers: [
        {
          provider: 'github',
          repository: 'acme/handbook',
          workflow: 'acme/handbook/.github/workflows/release.yml',
          ref: 'refs/heads/main',
          environment: 'release',
        },
      ],
    });
    expect(mayPublish(ci({ environment: 'release' }), pinned)).toBe(true);
    expect(mayPublish(ci(), pinned)).toBe(false);
    expect(mayPublish(ci({ environment: 'release', ref: 'refs/heads/feature' }), pinned)).toBe(false);
    expect(
      mayPublish(ci({ environment: 'release', workflow: 'acme/handbook/.github/workflows/other.yml' }), pinned),
    ).toBe(false);
    expect(mayPublish(ci({ environment: 'release', workflow: null }), pinned)).toBe(false);
  });
});

describe('mayCreate', () => {
  const config = { creators: ['owner@example.dev'], creatorDomains: ['acme.dev'] };

  it('allows listed creators and creator domains', () => {
    expect(mayCreate(user('owner@example.dev'), config)).toBe(true);
    expect(mayCreate(user('anyone@acme.dev'), config)).toBe(true);
    expect(mayCreate(user('anyone@notacme.dev'), config)).toBe(false);
  });

  it('denies everyone when the registry has no configuration, and always denies CI jobs', () => {
    expect(mayCreate(user('owner@example.dev'), undefined)).toBe(false);
    expect(mayCreate(ci(), config)).toBe(false);
  });
});

describe('input parsing', () => {
  it('normalises and de-duplicates emails', () => {
    expect(parseEmails([' A@Example.dev', 'a@example.dev'], 'readers', 10)).toEqual(['a@example.dev']);
    expect(() => parseEmails(['nope'], 'readers', 10)).toThrow('not an email address');
    expect(() => parseEmails('a@example.dev', 'readers', 10)).toThrow('list of strings');
    expect(() => parseEmails(['a@b.dev', 'c@d.dev'], 'readers', 1)).toThrow('at most 1');
  });

  it('validates domains', () => {
    expect(parseDomains(['Example.DEV'], 'readerDomains', 10)).toEqual(['example.dev']);
    expect(() => parseDomains(['@example.dev'], 'readerDomains', 10)).toThrow('not a domain');
  });

  it('validates trusted publishers', () => {
    expect(
      parseTrustedPublishers([{ provider: 'github', repository: 'Acme/Handbook', ref: 'refs/heads/main', workflow: '' }]),
    ).toEqual([{ provider: 'github', repository: 'acme/handbook', ref: 'refs/heads/main' }]);
    expect(() => parseTrustedPublishers([{ provider: 'gitlab', repository: 'a/b' }])).toThrow('provider');
    expect(() => parseTrustedPublishers([{ provider: 'github', repository: 'no-slash' }])).toThrow('owner/repo');
  });
});
