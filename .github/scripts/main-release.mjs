import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

export function workspaceVersion(manifest) {
  const version = manifest.match(/\[workspace\.package\][^\[]*?\nversion = "([^"]+)"/)?.[1];
  assert.ok(version, 'Expected a workspace package version in Cargo.toml');
  return version;
}

function semver(version) {
  const match = version.match(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/);
  assert.ok(match, `Expected a release version without build metadata: ${version}`);
  const pre = match[4]?.split('.') ?? [];
  assert.ok(pre.every(part => !/^\d+$/.test(part) || part === '0' || !part.startsWith('0')),
    `Invalid numeric prerelease identifier: ${version}`);
  return { core: match.slice(1, 4).map(BigInt), pre };
}

export function isVersionIncrease(previous, current) {
  const a = semver(previous);
  const b = semver(current);
  for (let i = 0; i < 3; i++) {
    if (a.core[i] !== b.core[i]) return b.core[i] > a.core[i];
  }
  if (!a.pre.length || !b.pre.length) return !!a.pre.length && !b.pre.length;
  for (let i = 0; i < Math.max(a.pre.length, b.pre.length); i++) {
    if (a.pre[i] === undefined) return true;
    if (b.pre[i] === undefined) return false;
    if (a.pre[i] === b.pre[i]) continue;
    const an = /^\d+$/.test(a.pre[i]);
    const bn = /^\d+$/.test(b.pre[i]);
    if (an && bn) return BigInt(b.pre[i]) > BigInt(a.pre[i]);
    if (an !== bn) return an;
    return b.pre[i] > a.pre[i];
  }
  return false;
}

export function shouldRelease({ branch, reason, testReleasePr, previous, current }) {
  if (branch !== 'refs/heads/main' || testReleasePr !== '0') return false;
  if (!['IndividualCI', 'BatchedCI', 'Manual'].includes(reason)) return false;
  return isVersionIncrease(previous, current);
}

function git(...args) {
  const result = spawnSync('git', args, { encoding: 'utf8' });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `git ${args.join(' ')} failed: ${result.stderr}`);
  return result.stdout.trim();
}

function main() {
  const branch = process.env.BUILD_SOURCEBRANCH;
  const reason = process.env.BUILD_REASON;
  const testReleasePr = process.env.TEST_RELEASE_PR;
  let publish = false;
  if (branch === 'refs/heads/main' && testReleasePr === '0' &&
      ['IndividualCI', 'BatchedCI', 'Manual'].includes(reason)) {
    assert.equal(git('rev-parse', 'HEAD'), process.env.BUILD_SOURCEVERSION,
      'Release detection must use the exact queued commit');
    const current = workspaceVersion(git('show', 'HEAD:Cargo.toml'));
    // The first parent is the previous main tip, including for merge commits.
    const previous = workspaceVersion(git('show', 'HEAD^1:Cargo.toml'));
    publish = shouldRelease({ branch, reason, testReleasePr, previous, current });
    console.log(`Main version ${previous} -> ${current}: ${publish ? 'publish signed release' : 'no release'}`);
  }
  console.log(`##vso[task.setvariable variable=PublishRelease;isOutput=true]${publish}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
