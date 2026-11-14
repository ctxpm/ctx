import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { ManifestError, parseManifest } from '../../src/manifest.js';

const valid = { schemaVersion: 1, name: 'team-handbook', version: '1.2.3', type: 'team' };

function problems(data: unknown): string[] {
  try {
    parseManifest(typeof data === 'string' ? data : JSON.stringify(data));
  } catch (error) {
    if (error instanceof ManifestError) {
      return error.problems;
    }
    throw error;
  }
  return [];
}

describe('parseManifest', () => {
  it('accepts a minimal manifest and fills defaults', () => {
    expect(parseManifest(JSON.stringify(valid))).toEqual({ ...valid, description: '', dependencies: {} });
  });

  it('accepts dependencies, pre-release versions and unknown fields', () => {
    const parsed = parseManifest(
      JSON.stringify({
        ...valid,
        version: '2.0.0-rc.1',
        dependencies: { 'frontend-standards': '^1.0.0', base: '*' },
        provides: { skills: [] },
        somethingNew: true,
      }),
    );
    expect(parsed.version).toBe('2.0.0-rc.1');
    expect(parsed.dependencies).toEqual({ 'frontend-standards': '^1.0.0', base: '*' });
  });

  it('accepts every example package in the repository', () => {
    const examples = join(import.meta.dirname, '../../../../examples');
    const names = readdirSync(examples);
    expect(names.length).toBeGreaterThan(0);
    for (const name of names) {
      const parsed = parseManifest(readFileSync(join(examples, name, 'manifest.json'), 'utf8'));
      expect(parsed.name).toBe(name);
    }
  });

  it.each([
    ['not json', 'not valid JSON'],
    ['[]', 'must be a JSON object'],
  ])('rejects %s', (raw, expected) => {
    expect(problems(raw)).toEqual([expected]);
  });

  it.each([
    [{ ...valid, schemaVersion: 2 }, 'schemaVersion'],
    [{ ...valid, name: 'Team_Handbook' }, 'name'],
    [{ ...valid, name: '__proto__' }, 'name'],
    [{ ...valid, name: 'a'.repeat(65) }, 'name'],
    [{ ...valid, name: '-leading' }, 'name'],
    [{ ...valid, version: 'v1.2.3' }, 'version'],
    [{ ...valid, version: '1.2' }, 'version'],
    [{ ...valid, version: ' 1.2.3' }, 'version'],
    [{ ...valid, type: 'Team' }, 'type'],
    [{ ...valid, description: 5 }, 'description'],
    [{ ...valid, dependencies: [] }, 'dependencies'],
    [{ ...valid, dependencies: { 'Bad Name': '1.0.0' } }, 'dependency name'],
    [{ ...valid, dependencies: { other: 'not a range' } }, 'invalid version range'],
    [{ ...valid, dependencies: { 'team-handbook': '*' } }, 'cannot depend on itself'],
    [{ ...valid, dependencies: { other: '^1.0.0 || ^2.0.0' } }, 'not supported'],
    [{ ...valid, dependencies: { other: '1.0.0 - 2.0.0' } }, 'not supported'],
    [{ ...valid, provides: 'x' }, 'provides'],
  ])('rejects %j', (data, expected) => {
    const found = problems(data);
    expect(found).toHaveLength(1);
    expect(found[0]).toContain(expected);
  });

  it('reports every problem at once', () => {
    expect(problems({ schemaVersion: 1 })).toHaveLength(3);
  });
});
