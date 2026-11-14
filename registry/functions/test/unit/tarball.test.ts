import { gzipSync } from 'node:zlib';
import { describe, expect, it } from 'vitest';
import { DEFAULT_LIMITS, inspectTarball, TarballError } from '../../src/tarball.js';
import { manifest, packageTgz, tgz } from '../tarballs.js';

describe('inspectTarball', () => {
  it('reads manifest, readme and file list from a root-level archive', async () => {
    const info = await inspectTarball(await packageTgz('alpha', '1.0.0'));
    expect(info.prefix).toBe('');
    expect(JSON.parse(info.manifestRaw).name).toBe('alpha');
    expect(info.readme).toBe('# alpha\n');
    expect(info.files.map((file) => file.path)).toEqual(['docs/guide.md', 'manifest.json', 'README.md']);
    expect(info.unpackedSize).toBeGreaterThan(0);
  });

  it('strips a single top-level directory, as produced by npm pack', async () => {
    const info = await inspectTarball(await packageTgz('alpha', '1.0.0', { prefix: 'package/' }));
    expect(info.prefix).toBe('package/');
    expect(info.files.map((file) => file.path)).toContain('manifest.json');
    expect(info.readme).toBe('# alpha\n');
  });

  it('ignores a manifest.json that is not at the package root', async () => {
    const archive = await tgz([
      { name: 'a/manifest.json', content: manifest('alpha', '1.0.0') },
      { name: 'b/readme.md', content: 'x' },
    ]);
    await expect(inspectTarball(archive)).rejects.toThrow('manifest.json not found');
  });

  it('rejects symbolic links', async () => {
    const archive = await tgz([
      { name: 'manifest.json', content: manifest('alpha', '1.0.0') },
      { name: 'link', type: 'symlink', linkname: '/etc/passwd' },
    ]);
    await expect(inspectTarball(archive)).rejects.toThrow('Unsupported entry type');
  });

  it.each(['../escape.md', '/abs.md', 'docs/../../escape.md'])('rejects the path %s', async (name) => {
    const archive = await tgz([
      { name: 'manifest.json', content: manifest('alpha', '1.0.0') },
      { name, content: 'x' },
    ]);
    await expect(inspectTarball(archive)).rejects.toBeInstanceOf(TarballError);
  });

  it('rejects data that is not a gzip archive', async () => {
    await expect(inspectTarball(Buffer.from('not an archive'))).rejects.toThrow('Not a readable .tgz');
  });

  it('rejects an empty archive', async () => {
    await expect(inspectTarball(await tgz([]))).rejects.toThrow('Archive is empty');
  });

  it('rejects an archive that unpacks beyond the size limit', async () => {
    const archive = await tgz([
      { name: 'manifest.json', content: manifest('alpha', '1.0.0') },
      { name: 'big.md', content: 'x'.repeat(5000) },
    ]);
    await expect(inspectTarball(archive, { ...DEFAULT_LIMITS, maxUnpackedBytes: 1000 })).rejects.toThrow(
      'unpacks to more than',
    );
  });

  it('rejects an archive with too many entries', async () => {
    const archive = await tgz([
      { name: 'manifest.json', content: manifest('alpha', '1.0.0') },
      { name: 'a.md', content: 'a' },
      { name: 'b.md', content: 'b' },
    ]);
    await expect(inspectTarball(archive, { ...DEFAULT_LIMITS, maxEntries: 2 })).rejects.toThrow('more than 2 entries');
  });

  it('rejects the same path twice', async () => {
    const archive = await tgz([
      { name: 'manifest.json', content: manifest('alpha', '1.0.0') },
      { name: 'a.md', content: 'one' },
      { name: './a.md', content: 'two' },
    ]);
    await expect(inspectTarball(archive)).rejects.toThrow('Duplicate path');
  });

  it('rejects a manifest larger than the limit', async () => {
    const archive = await tgz([{ name: 'manifest.json', content: manifest('alpha', '1.0.0', { pad: 'x'.repeat(2000) }) }]);
    await expect(inspectTarball(archive, { ...DEFAULT_LIMITS, maxManifestBytes: 500 })).rejects.toThrow(
      'manifest.json is empty or larger',
    );
  });

  it('rejects a truncated archive', async () => {
    const archive = await packageTgz('alpha', '1.0.0');
    await expect(inspectTarball(archive.subarray(0, archive.length - 20))).rejects.toBeInstanceOf(TarballError);
  });

  it('rejects gzip data that is not a tar archive', async () => {
    await expect(inspectTarball(gzipSync(Buffer.from('plain text, no tar header')))).rejects.toBeInstanceOf(TarballError);
  });
});
