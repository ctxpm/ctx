import { gzipSync } from 'node:zlib';
import tar from 'tar-stream';

export interface Entry {
  name: string;
  content?: string;
  type?: 'file' | 'directory' | 'symlink';
  linkname?: string;
}

/** Builds a .tgz in memory from a list of entries. */
export async function tgz(entries: Entry[]): Promise<Buffer> {
  const pack = tar.pack();
  const chunks: Buffer[] = [];
  pack.on('data', (chunk: unknown) => chunks.push(chunk as Buffer));
  const done = new Promise<void>((resolve, reject) => {
    pack.on('end', resolve);
    pack.on('error', reject);
  });
  for (const entry of entries) {
    if (entry.type === 'symlink') {
      pack.entry({ name: entry.name, type: 'symlink', linkname: entry.linkname ?? '' });
    } else if (entry.type === 'directory') {
      pack.entry({ name: entry.name, type: 'directory' });
    } else {
      pack.entry({ name: entry.name }, entry.content ?? '');
    }
  }
  pack.finalize();
  await done;
  return gzipSync(Buffer.concat(chunks));
}

export function manifest(name: string, version: string, extra: Record<string, unknown> = {}): string {
  return JSON.stringify({ schemaVersion: 1, name, version, type: 'project', description: `${name} test package`, ...extra });
}

/** A minimal valid package archive. */
export function packageTgz(
  name: string,
  version: string,
  options: { extra?: Record<string, unknown>; prefix?: string; files?: Record<string, string> } = {},
): Promise<Buffer> {
  const prefix = options.prefix ?? '';
  const files = { 'README.md': `# ${name}\n`, 'docs/guide.md': 'guide\n', ...(options.files ?? {}) };
  return tgz([
    { name: `${prefix}manifest.json`, content: manifest(name, version, options.extra) },
    ...Object.entries(files).map(([path, content]) => ({ name: `${prefix}${path}`, content })),
  ]);
}
