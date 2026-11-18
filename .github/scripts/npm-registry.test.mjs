// The official ADO build's agents run under network isolation, which blocks registry.npmjs.org but
// not the anonymous 1es-public feed. Every committed lockfile resolves from that feed, and each
// package's .npmrc keeps a regenerated lockfile there whatever the user's own registry is.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const FEED = 'https://ms-feed-25.pkgs.visualstudio.com/1es-public/_packaging/npm-public/npm/registry/';

const lockfiles = execFileSync('git', ['ls-files', '*package-lock.json'], { cwd: ROOT, encoding: 'utf8' })
  .split('\n')
  .filter(Boolean);

test('lockfiles resolve every package from the 1es-public feed', () => {
  assert.ok(lockfiles.length > 0, 'no package-lock.json files found');
  const elsewhere = lockfiles.flatMap((file) =>
    Object.entries(JSON.parse(readFileSync(path.join(ROOT, file), 'utf8')).packages ?? {})
      .filter(([, entry]) => entry.resolved && !entry.resolved.startsWith(FEED))
      .map(([name, entry]) => `${file}: ${name} -> ${entry.resolved}`),
  );
  assert.deepEqual(elsewhere, [], `regenerate these with npm's registry set to ${FEED}`);
});

test('each package with a lockfile pins npm to the 1es-public feed', () => {
  const unpinned = lockfiles
    .map((file) => path.join(path.dirname(file), '.npmrc'))
    .filter((npmrc) => {
      const at = path.join(ROOT, npmrc);
      return !existsSync(at) || !readFileSync(at, 'utf8').split(/\r?\n/).includes(`registry=${FEED}`);
    });
  assert.deepEqual(unpinned, [], `add registry=${FEED} to these`);
});
