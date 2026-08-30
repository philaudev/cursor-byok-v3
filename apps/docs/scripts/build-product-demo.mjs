import { existsSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const docsRoot = path.dirname(fileURLToPath(new URL('../package.json', import.meta.url)));
const desktopRoot = path.resolve(docsRoot, '../desktop');
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
const vite = path.join(desktopRoot, 'node_modules', '.bin', process.platform === 'win32' ? 'vite.cmd' : 'vite');

if (!existsSync(vite)) run(['ci'], desktopRoot);
run(['run', 'build:demo'], desktopRoot);

function run(args, cwd) {
  const result = spawnSync(npm, args, { cwd, stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
