import semver from 'semver';

/** The parts of manifest.json the registry understands. Mirrors spec/manifest.schema.json. */
export interface Manifest {
  schemaVersion: 1;
  name: string;
  version: string;
  type: string;
  description: string;
  dependencies: Record<string, string>;
}

export const PACKAGE_NAME = /^[a-z0-9]+(-[a-z0-9]+)*$/;
export const PACKAGE_TYPE = /^[a-z][a-z0-9-]{0,31}$/;
const MAX_NAME_LENGTH = 64;
const MAX_DEPENDENCIES = 200;

export class ManifestError extends Error {
  constructor(readonly problems: string[]) {
    super(`Invalid manifest.json: ${problems.join('; ')}`);
    this.name = 'ManifestError';
  }
}

export function isPackageName(value: unknown): value is string {
  return typeof value === 'string' && value.length <= MAX_NAME_LENGTH && PACKAGE_NAME.test(value);
}

/** An exact semver version in canonical form (no leading "v", no surrounding whitespace). */
export function isExactVersion(value: unknown): value is string {
  return typeof value === 'string' && value.length <= 64 && semver.valid(value) === value;
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Parses and validates the raw text of a manifest.json. Throws ManifestError listing every problem. */
export function parseManifest(raw: string): Manifest {
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    throw new ManifestError(['not valid JSON']);
  }
  if (!isPlainObject(data)) {
    throw new ManifestError(['must be a JSON object']);
  }

  const problems: string[] = [];

  if (data.schemaVersion !== 1) {
    problems.push('schemaVersion must be 1');
  }
  if (!isPackageName(data.name)) {
    problems.push('name must be lowercase kebab-case, at most 64 characters');
  }
  if (!isExactVersion(data.version)) {
    problems.push('version must be an exact semantic version such as 1.2.3');
  }
  if (typeof data.type !== 'string' || !PACKAGE_TYPE.test(data.type)) {
    problems.push('type must be a lowercase identifier such as "project"');
  }
  if (data.description !== undefined && (typeof data.description !== 'string' || data.description.length > 500)) {
    problems.push('description must be a string of at most 500 characters');
  }
  for (const field of ['provides', 'engines', 'provenance'] as const) {
    if (data[field] !== undefined && !isPlainObject(data[field])) {
      problems.push(`${field} must be an object`);
    }
  }

  const dependencies: Record<string, string> = {};
  if (data.dependencies !== undefined) {
    if (!isPlainObject(data.dependencies)) {
      problems.push('dependencies must be an object');
    } else {
      const entries = Object.entries(data.dependencies);
      if (entries.length > MAX_DEPENDENCIES) {
        problems.push(`at most ${MAX_DEPENDENCIES} dependencies`);
      }
      for (const [name, range] of entries) {
        if (!isPackageName(name)) {
          problems.push(`dependency name "${name}" is not a valid package name`);
        } else if (name === data.name) {
          problems.push('a package cannot depend on itself');
        } else if (typeof range !== 'string' || range.length > 128 || semver.validRange(range) === null) {
          problems.push(`dependency "${name}" has an invalid version range`);
        } else {
          dependencies[name] = range;
        }
      }
    }
  }

  if (problems.length > 0) {
    throw new ManifestError(problems);
  }

  return {
    schemaVersion: 1,
    name: data.name as string,
    version: data.version as string,
    type: data.type as string,
    description: (data.description as string | undefined) ?? '',
    dependencies,
  };
}
