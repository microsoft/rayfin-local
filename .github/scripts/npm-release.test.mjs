import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { extractExecutable } from '../../scripts/pack-npm-release.mjs';

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'rayfin-npm-test-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const version = '0.1.0';
  const target = 'x86_64-unknown-linux-gnu';
  const name = `rayfin-local-${version}-${target}`;
  const bytes = Buffer.from([0x7f, 0x45, 0x4c, 0x46, 0x00, 0xff]);
  const binarySha256 = createHash('sha256').update(bytes).digest('hex');
  mkdirSync(join(directory, name));
  writeFileSync(join(directory, name, 'rayfin-local'), bytes);
  const archive = join(directory, 'release.tar.gz');
  execFileSync('tar', ['-czf', archive, '-C', directory, name]);
  return { bytes, archive, metadata: { version, target, binarySha256 }, destination: join(directory, 'bin', 'rayfin-local') };
}

test('preserves verified executable bytes and restores the Unix execute mode', (t) => {
  const { bytes, archive, metadata, destination } = fixture(t);
  extractExecutable(archive, metadata, destination);
  assert.deepEqual(readFileSync(destination), bytes);
  if (process.platform !== 'win32') assert.equal(statSync(destination).mode & 0o777, 0o755);
});

test('does not stage an executable that differs from signing metadata', (t) => {
  const { archive, metadata, destination } = fixture(t);
  assert.throws(() => extractExecutable(archive, { ...metadata, binarySha256: '0'.repeat(64) }, destination), /executable hash mismatch/);
  assert.equal(existsSync(destination), false);
});
