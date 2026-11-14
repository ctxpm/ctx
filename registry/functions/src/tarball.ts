import { createGunzip } from 'node:zlib';
import tar from 'tar-stream';

export interface TarballFile {
  path: string;
  size: number;
}

export interface TarballInfo {
  /** Raw text of manifest.json. */
  manifestRaw: string;
  /** Text of the root README.md, if present and within the size limit. */
  readme: string | null;
  /** Regular files, with the top-level directory stripped when there is one. */
  files: TarballFile[];
  unpackedSize: number;
  /** Top-level directory every entry lives under ("" when the manifest is at the root). */
  prefix: string;
}

export interface TarballLimits {
  maxEntries: number;
  maxUnpackedBytes: number;
  maxManifestBytes: number;
  maxReadmeBytes: number;
}

export const DEFAULT_LIMITS: TarballLimits = {
  maxEntries: 10_000,
  maxUnpackedBytes: 200 * 1024 * 1024,
  maxManifestBytes: 256 * 1024,
  maxReadmeBytes: 256 * 1024,
};

export class TarballError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'TarballError';
  }
}

/** Normalises an entry path and rejects anything that could escape the install directory. */
function safePath(raw: string): string {
  if (raw.includes('\0') || raw.includes('\\')) {
    throw new TarballError(`Unsafe path in archive: ${JSON.stringify(raw)}`);
  }
  if (raw.startsWith('/')) {
    throw new TarballError(`Absolute path in archive: ${raw}`);
  }
  const segments = raw.split('/').filter((segment) => segment !== '' && segment !== '.');
  if (segments.includes('..')) {
    throw new TarballError(`Path with ".." in archive: ${raw}`);
  }
  return segments.join('/');
}

/**
 * Reads a .tgz in one pass without writing anything to disk: checks every entry, collects
 * the file list and keeps the manifest and README text.
 */
export async function inspectTarball(data: Buffer, limits: TarballLimits = DEFAULT_LIMITS): Promise<TarballInfo> {
  const extract = tar.extract();
  const gunzip = createGunzip();
  let failure: TarballError | undefined;
  let settle: (() => void) | undefined;
  /** Stops reading at the first problem, so a hostile archive is not decompressed to the end. */
  const fail = (error: TarballError): void => {
    if (failure === undefined) {
      failure = error;
      gunzip.destroy();
      extract.destroy();
      settle?.();
    }
  };
  const entries: { path: string; size: number; isFile: boolean }[] = [];
  /** Small files kept in memory because they may turn out to be the manifest or README. */
  const kept = new Map<string, Buffer>();
  let unpackedSize = 0;

  extract.on('entry', (header, stream, next) => {
    let path: string;
    try {
      if (header.type !== 'file' && header.type !== 'directory') {
        throw new TarballError(`Unsupported entry type "${header.type}" for ${header.name}; only files and directories are allowed`);
      }
      path = safePath(header.name);
      if (entries.length >= limits.maxEntries) {
        throw new TarballError(`Archive has more than ${limits.maxEntries} entries`);
      }
    } catch (error) {
      stream.on('error', () => undefined);
      stream.resume();
      fail(error as TarballError);
      return;
    }

    const isFile = header.type === 'file';
    const depth = path.split('/').length;
    const base = path.split('/').pop() ?? '';
    const keepLimit =
      base === 'manifest.json' ? limits.maxManifestBytes : base.toLowerCase() === 'readme.md' ? limits.maxReadmeBytes : 0;
    const keep = isFile && depth <= 2 && keepLimit > 0;
    const chunks: Buffer[] = [];
    let size = 0;
    let overflow = false;

    stream.on('data', (data: unknown) => {
      const chunk = data as Buffer;
      size += chunk.length;
      unpackedSize += chunk.length;
      if (unpackedSize > limits.maxUnpackedBytes) {
        fail(new TarballError(`Archive unpacks to more than ${limits.maxUnpackedBytes} bytes`));
        return;
      }
      if (keep && !overflow) {
        if (size > keepLimit) {
          overflow = true;
          chunks.length = 0;
        } else {
          chunks.push(chunk);
        }
      }
    });
    stream.on('error', () => undefined);
    stream.on('end', () => {
      if (path !== '') {
        entries.push({ path, size, isFile });
        if (keep && !overflow) {
          kept.set(path, Buffer.concat(chunks));
        } else if (keep && base === 'manifest.json') {
          kept.set(path, Buffer.alloc(0));
        }
      }
      next();
    });
    stream.resume();
  });

  await new Promise<void>((resolve) => {
    settle = resolve;
    const unreadable = (error: Error): void => fail(new TarballError(`Not a readable .tgz archive: ${error.message}`));
    gunzip.on('error', unreadable);
    extract.on('error', unreadable);
    extract.on('finish', resolve);
    gunzip.pipe(extract);
    gunzip.end(data);
  });
  if (failure !== undefined) {
    throw failure;
  }

  if (entries.length === 0) {
    throw new TarballError('Archive is empty');
  }

  let prefix = '';
  if (!kept.has('manifest.json')) {
    const top = new Set(entries.map((entry) => entry.path.split('/')[0]));
    const only = top.size === 1 ? [...top][0] : undefined;
    if (only === undefined || !kept.has(`${only}/manifest.json`)) {
      throw new TarballError('manifest.json not found at the archive root');
    }
    prefix = `${only}/`;
  }

  const manifest = kept.get(`${prefix}manifest.json`);
  if (manifest === undefined || manifest.length === 0) {
    throw new TarballError(`manifest.json is empty or larger than ${limits.maxManifestBytes} bytes`);
  }

  const files = entries
    .filter((entry) => entry.isFile)
    .map((entry) => ({ path: entry.path.slice(prefix.length), size: entry.size }))
    .sort((a, b) => a.path.localeCompare(b.path));

  const duplicates = files.filter((file, index) => index > 0 && files[index - 1]?.path === file.path);
  if (duplicates.length > 0) {
    throw new TarballError(`Duplicate path in archive: ${duplicates[0]?.path}`);
  }

  const readmeKey = [...kept.keys()].find(
    (key) => key.startsWith(prefix) && key.slice(prefix.length).toLowerCase() === 'readme.md',
  );
  const readme = readmeKey === undefined ? null : (kept.get(readmeKey)?.toString('utf8') ?? null);

  return {
    manifestRaw: manifest.toString('utf8'),
    readme,
    files,
    unpackedSize,
    prefix,
  };
}
