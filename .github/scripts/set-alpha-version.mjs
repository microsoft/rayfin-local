import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';

export function alphaManifest(manifest, buildId) {
  assert.match(buildId, /^[1-9][0-9]*$/, 'An official ADO build ID is required');
  const pattern = /(\[workspace\.package\][^\[]*?\nversion = ")(\d+\.\d+\.\d+)(?:-[0-9A-Za-z.-]+)?(")/;
  const match = manifest.match(pattern);
  assert.ok(match, 'Expected one workspace package version in Cargo.toml');
  const version = `${match[2]}-alpha.${buildId}`;
  return { version, manifest: manifest.replace(pattern, `$1${version}$3`) };
}

async function main() {
  const root = new URL('../../', import.meta.url);
  const path = new URL('Cargo.toml', root);
  const result = alphaManifest(await readFile(path, 'utf8'), process.argv[2] ?? '');
  await writeFile(path, result.manifest);
  const cargo = spawnSync('cargo', ['update', '--workspace'], { cwd: root, stdio: 'inherit' });
  if (cargo.error) throw cargo.error;
  assert.equal(cargo.status, 0, 'Cannot update workspace versions in Cargo.lock');
  console.log(`Building test release ${result.version}; source files are changed only in this build checkout.`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await main();
}
