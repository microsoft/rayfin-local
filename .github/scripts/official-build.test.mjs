// What the official ADO build needs from the repository that GitHub CI wouldn't otherwise notice:
// its agents run under network isolation, which blocks public package registries.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const read = (file) => readFileSync(path.join(ROOT, file), 'utf8');

const FEED = 'https://pkgs.dev.azure.com/powerbi/AppDev/_packaging/rayfin-official/npm/registry/';

// registry.npmjs.org URLs are fine: npm swaps in the configured registry, the feed, for them. The
// agents reach 1es-public directly. npm keeps any other host, and the agents can't reach it.
const LOCKFILE_REGISTRIES = [
  'https://registry.npmjs.org/',
  'https://ms-feed-25.pkgs.visualstudio.com/1es-public/_packaging/npm-public/npm/registry/',
];

test('lockfiles resolve packages only from registries the official build reaches', () => {
  const lockfiles = execFileSync('git', ['ls-files', '*package-lock.json'], { cwd: ROOT, encoding: 'utf8' })
    .split('\n')
    .filter(Boolean);
  assert.ok(lockfiles.length > 0, 'no package-lock.json files found');
  const unreachable = lockfiles.flatMap((file) =>
    Object.entries(JSON.parse(read(file)).packages ?? {})
      .filter(([, entry]) => entry.resolved && !LOCKFILE_REGISTRIES.some((r) => entry.resolved.startsWith(r)))
      .map(([name, entry]) => `${file}: ${name} -> ${entry.resolved}`),
  );
  assert.deepEqual(unreachable, [], 'regenerate these entries against registry.npmjs.org');
});

/** The jobs in an Azure Pipelines file that run `cargo test`, ignoring commented-out ones. */
function jobsRunningCargoTest(file) {
  const live = read(file)
    .split(/\r?\n/)
    .filter((line) => !line.trimStart().startsWith('#'))
    .join('\n');
  return live
    .split(/^\s*- job:/m)
    .slice(1)
    .filter((job) => /\bcargo test\b/.test(job));
}

test('official build jobs install the fixtures through the internal feed before testing', () => {
  for (const file of ['.azure-pipelines/rayfin-local.official.yml', '.azure-pipelines/templates/unix-job.yml']) {
    const jobs = jobsRunningCargoTest(file);
    assert.ok(jobs.length > 0, `${file}: no job runs cargo test`);
    for (const job of jobs) {
      const name = job.split('\n')[0].trim();
      const setup = job.slice(0, job.search(/\bcargo test\b/));
      assert.ok(setup.includes(`registry=${FEED}`), `${file} ${name}: npm isn't pointed at the feed`);
      assert.ok(setup.includes('variable=NPM_CONFIG_USERCONFIG]'), `${file} ${name}: npm doesn't read the feed config`);
      assert.match(
        setup,
        /task: npmAuthenticate@0\s+(?:displayName: [^\n]+\s+)?inputs:\s+workingFile: \$\(Agent\.TempDirectory\)\/rayfin-official\.npmrc/,
        `${file} ${name}: npm isn't authenticated to the feed`,
      );
    }
  }
});
