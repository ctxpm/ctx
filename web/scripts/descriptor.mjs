// Puts a registry descriptor where the dev server serves it. The app reads all of its
// configuration from /.well-known/ctx-registry.json at runtime, so the same build works
// for any registry. Default: the descriptor of the local Firebase emulators.
import { copyFileSync, mkdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const source = resolve(process.env.CTX_DESCRIPTOR ?? resolve(here, '../../registry/emulator-registry.json'));
const target = resolve(here, '../public/.well-known/ctx-registry.json');

mkdirSync(dirname(target), { recursive: true });
copyFileSync(source, target);
console.log(`descriptor: ${source}`);
