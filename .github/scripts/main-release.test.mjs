import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { isVersionIncrease, shouldRelease, workspaceVersion } from './main-release.mjs';

test('only increasing main versions publish, including manual retries', () => {
  const input = {
    branch: 'refs/heads/main', reason: 'IndividualCI', testReleasePr: '0',
    previous: '0.2.8', current: '0.2.9',
  };
  assert.equal(shouldRelease(input), true);
  assert.equal(shouldRelease({ ...input, reason: 'Manual' }), true);
  assert.equal(shouldRelease({ ...input, current: '0.2.8' }), false);
  assert.equal(shouldRelease({ ...input, current: '0.2.7' }), false);
  assert.equal(shouldRelease({ ...input, branch: 'refs/heads/feature' }), false);
  assert.equal(shouldRelease({ ...input, branch: 'refs/tags/v0.2.9' }), false);
  assert.equal(shouldRelease({ ...input, reason: 'PullRequest' }), false);
  assert.equal(shouldRelease({ ...input, testReleasePr: '11' }), false);
});

test('version precedence handles numeric components and prerelease promotion', () => {
  for (const [a, b] of [
    ['0.2.9', '0.2.10'], ['0.2.8', '0.3.0'], ['0.9.9', '1.0.0'],
    ['0.2.9-rc.2', '0.2.9-rc.10'], ['0.2.9-rc.10', '0.2.9'],
    ['0.2.9-alpha', '0.2.9-beta'], ['0.2.9-1', '0.2.9-alpha'],
    ['0.2.9-alpha', '0.2.9-alpha.1'],
  ]) {
    assert.equal(isVersionIncrease(a, b), true, `${a} -> ${b}`);
    assert.equal(isVersionIncrease(b, a), false, `${b} -> ${a}`);
    assert.equal(isVersionIncrease(a, a), false);
  }
  for (const invalid of ['01.2.3', '1.2', '1.2.3-01', '1.2.3+build', '$(Version)']) {
    assert.throws(() => isVersionIncrease('0.2.8', invalid));
  }
});

test('reads workspace version rather than dependency versions', () => {
  assert.equal(workspaceVersion('[workspace.package]\nversion = "0.2.9"\n[workspace.dependencies]\nx = "1"'), '0.2.9');
  assert.throws(() => workspaceVersion('[package]\nversion = "0.2.9"'));
});

test('CLI detects a merge version bump against the first parent and fails closed on wrong source', () => {
  const cwd = mkdtempSync(join(tmpdir(), 'rayfin-release-'));
  const script = fileURLToPath(new URL('./main-release.mjs', import.meta.url));
  function git(...args) {
    const result = spawnSync('git', ['-c', 'user.name=Release Test', '-c', 'user.email=test@example.invalid',
      '-c', 'commit.gpgsign=false', ...args], { cwd, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  }
  try {
    git('init', '-b', 'main');
    writeFileSync(join(cwd, 'Cargo.toml'), '[workspace.package]\nversion = "0.2.8"\n');
    git('add', '.');
    git('commit', '-m', 'Initial version');
    git('checkout', '-b', 'bump');
    writeFileSync(join(cwd, 'Cargo.toml'), '[workspace.package]\nversion = "0.2.9"\n');
    git('commit', '-am', 'Bump version');
    git('checkout', 'main');
    writeFileSync(join(cwd, 'README.md'), 'Unrelated main change\n');
    git('add', '.');
    git('commit', '-m', 'Unrelated change');
    git('merge', '--no-ff', 'bump', '-m', 'Merge version bump');
    const env = { ...process.env, BUILD_SOURCEBRANCH: 'refs/heads/main',
      BUILD_REASON: 'IndividualCI', TEST_RELEASE_PR: '0', BUILD_SOURCEVERSION: git('rev-parse', 'HEAD') };
    const run = overrides => spawnSync(process.execPath, [script], {
      cwd, env: { ...env, ...overrides }, encoding: 'utf8',
    });
    const result = run({});
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /variable=PublishRelease;isOutput=true]true/);
    assert.notEqual(run({ BUILD_SOURCEVERSION: 'a'.repeat(40) }).status, 0);
    git('commit', '--allow-empty', '-m', 'No version change');
    const unchanged = run({ BUILD_SOURCEVERSION: git('rev-parse', 'HEAD') });
    assert.equal(unchanged.status, 0, unchanged.stderr);
    assert.match(unchanged.stdout, /variable=PublishRelease;isOutput=true]false/);
  } finally {
    rmSync(cwd, { recursive: true, force: true });
  }
});
