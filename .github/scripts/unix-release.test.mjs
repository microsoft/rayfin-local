import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, mkdtempSync, writeFileSync, mkdirSync, rmSync, existsSync, statSync, copyFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { test } from 'node:test';
import { assemble } from './assemble-release.mjs';
import { verifyAppleIdentity, verifyGpgStatus, verifyLinuxElf, linuxFingerprint, targets } from './unix-release.mjs';

function elfFixture() {
  const binary = Buffer.alloc(64);
  binary.set([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1]);
  binary.writeUInt16LE(3, 16);
  binary.writeUInt16LE(62, 18);
  binary.writeUInt32LE(1, 20);
  return binary;
}

test('Linux cross-compiled executables must be x86-64 ELF64, not host binaries', () => {
  const binary = elfFixture();
  verifyLinuxElf(binary);
  assert.throws(() => verifyLinuxElf(binary.subarray(0, 63)));
  for (const [offset, value] of [[0, 0x4d], [4, 1], [5, 2], [6, 0], [16, 1], [18, 183], [20, 0]]) {
    const invalid = Buffer.from(binary);
    invalid[offset] = value;
    assert.throws(() => verifyLinuxElf(invalid));
  }
});

test('Linux requires the pinned valid signing key and a modern digest', () => {
  const status = `[GNUPG:] VALIDSIG ${linuxFingerprint} 2026-11-16 1794787200 0 4 0 1 8 00 ${linuxFingerprint}`;
  verifyGpgStatus(status);
  assert.throws(() => verifyGpgStatus('[GNUPG:] BADSIG EB3E94ADBE1229CF'));
  assert.throws(() => verifyGpgStatus(status.replaceAll(linuxFingerprint, 'A'.repeat(40))));
  assert.throws(() => verifyGpgStatus(status.replace(' 1 8 00 ', ' 1 2 00 ')));
  assert.throws(() => verifyGpgStatus(`${status}\n${status}`));
  for (const invalid of ['EXPKEYSIG', 'REVKEYSIG', 'EXPSIG']) {
    assert.throws(() => verifyGpgStatus(`${status}\n[GNUPG:] ${invalid} EB3E94ADBE1229CF`));
  }
});

test('checked-in official public key matches pinned fingerprint', () => {
  const text = readFileSync(new URL('../../.azure-pipelines/keys/microsoft.asc', import.meta.url), 'utf8');
  const packet = Buffer.from(text.split('\n').filter(line => /^[a-zA-Z0-9+/]/.test(line) &&
    !line.startsWith('Version:')).join(''), 'base64');
  assert.equal(packet[0], 0x99);
  const length = packet.readUInt16BE(1);
  assert.equal(createHash('sha1').update(packet.subarray(0, length + 3)).digest('hex').toUpperCase(), linuxFingerprint);
});

test('Apple identity must have expected Developer ID, team, hardened runtime and timestamp', () => {
  const details = 'CodeDirectory v=20500 size=123 flags=0x10000(runtime) hashes=1\n' +
    'Authority=Developer ID Application: Microsoft Corporation (UBF8T346G9)\n' +
    'TeamIdentifier=UBF8T346G9\nTimestamp=Nov 16, 2026\n';
  verifyAppleIdentity(details);
  for (const invalid of [
    details.replaceAll('UBF8T346G9', 'WRONGTEAM0'),
    details.replace('Timestamp=Nov 16, 2026\n', ''),
    details.replace('Timestamp=Nov 16, 2026', 'Timestamp=none'),
    details.replace('0x10000(runtime)', '0x0(none)'),
    details.replace('Developer ID Application: Microsoft Corporation', 'Apple Development: Microsoft Corporation'),
  ]) assert.throws(() => verifyAppleIdentity(invalid));
});

function checkPackaging(platform) {
  const root = mkdtempSync(join(tmpdir(), 'rayfin-package-'));
  const target = targets[platform];
  const version = '0.2.9';
  const script = fileURLToPath(new URL('./unix-release.mjs', import.meta.url));
  const marker = join(root, 'target-executed');
  const env = { ...process.env, RAYFIN_EXECUTION_MARKER: marker,
    BUILD_ARTIFACTSTAGINGDIRECTORY: root, AGENT_TEMPDIRECTORY: root,
    BUILD_SOURCEVERSION: 'a'.repeat(40), BUILD_BUILDID: '123',
  };
  try {
    const binaryDirectory = join(root, 'target', target, 'release');
    mkdirSync(binaryDirectory, { recursive: true });
    const builtBinary = join(binaryDirectory, 'rayfin-local');
    const cSource = join(root, 'fixture.c');
    if (platform === 'macos') {
      writeFileSync(cSource, '#include <stdio.h>\n#include <stdlib.h>\n' +
        'int main(void) { const char *p = getenv("RAYFIN_EXECUTION_MARKER"); ' +
        'if (p && *p) { FILE *f = fopen(p, "w"); if (!f) return 1; fclose(f); } ' +
        `puts("rayfin-local ${version}"); return 0; }\n`);
      const compilation = spawnSync('xcrun', ['--sdk', 'macosx', 'clang',
        '-target', 'arm64-apple-macos11', cSource, '-o', builtBinary], { encoding: 'utf8' });
      assert.equal(compilation.status, 0, compilation.stderr);
    } else writeFileSync(builtBinary, elfFixture(), { mode: 0o644 });
    const contents = readFileSync(builtBinary);
    writeFileSync(join(root, 'Cargo.toml'), `[workspace.package]\nversion = "${version}"\n`);
    for (const file of ['LICENSE', 'README.md']) writeFileSync(join(root, file), 'packaging fixture');
    const result = spawnSync(process.execPath, [script, 'package', platform],
      { cwd: root, encoding: 'utf8', env });
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
    const output = join(root, `unsigned-${platform}`);
    const metadata = JSON.parse(readFileSync(join(output, `${platform}-source.json`), 'utf8'));
    const archive = join(output, metadata.archiveFile);
    const extract = join(root, 'extract');
    mkdirSync(extract);
    const extraction = platform === 'macos'
      ? spawnSync('ditto', ['-x', '-k', archive, extract])
      : spawnSync('tar', ['-xzf', archive, '-C', extract]);
    assert.equal(extraction.status, 0);
    const binary = join(extract, `rayfin-local-${version}-${target}`, 'rayfin-local');
    assert.deepEqual(readFileSync(binary), contents);
    if (process.platform !== 'win32') assert.equal(statSync(binary).mode & 0o111, 0o111);
    assert.equal(metadata.nativeExecutionTested, false);
    assert.equal(createHash('sha256').update(readFileSync(binary)).digest('hex'), metadata.binarySha256);
    assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), metadata.archiveSha256);
    if (platform === 'linux') {
      const modes = spawnSync(process.platform === 'win32' ? 'python' : 'python3', ['-c',
        'import sys,tarfile; t=tarfile.open(sys.argv[1]); ' +
        'assert t.getmember(sys.argv[2]+"/rayfin-local").mode == 0o755; ' +
        'assert t.getmember(sys.argv[2]+"/README.md").mode == 0o644',
        archive, `rayfin-local-${version}-${target}`], { encoding: 'utf8' });
      assert.equal(modes.status, 0, modes.stderr);
      writeFileSync(builtBinary, '#!/bin/sh\necho not-an-ELF\n');
      const wrongTarget = spawnSync(process.execPath, [script, 'package', platform],
        { cwd: root, encoding: 'utf8', env });
      assert.notEqual(wrongTarget.status, 0);
      assert.match(wrongTarget.stderr, /ELF64/);
    }
    if (platform === 'macos') {
      assert.equal(metadata.nativeExecutionTested, false);
      assert.equal(existsSync(marker), false, 'Packaging must not execute the ARM64 target');
      const input = join(root, 'unix-signing');
      mkdirSync(input);
      for (const file of [metadata.archiveFile, 'macos-source.json']) {
        copyFileSync(join(output, file), join(input, file));
      }
      const verification = spawnSync(process.execPath, [script, 'verify', platform], {
        cwd: root, encoding: 'utf8', env: { ...env, PIPELINE_WORKSPACE: root, RAYFIN_EXECUTION_MARKER: marker },
      });
      assert.notEqual(verification.status, 0, 'An ad-hoc signed fixture must not pass Developer ID verification');
      assert.match(verification.stderr, /Unexpected Apple Team ID|codesign failed/);
      assert.equal(existsSync(marker), false, 'Signature verification must not execute the ARM64 target');
      const intel = spawnSync('xcrun', ['--sdk', 'macosx', 'clang',
        '-target', 'x86_64-apple-macos11', cSource, '-o', builtBinary], { encoding: 'utf8' });
      assert.equal(intel.status, 0, intel.stderr);
      const wrongTarget = spawnSync(process.execPath, [script, 'package', platform],
        { cwd: root, encoding: 'utf8', env });
      assert.notEqual(wrongTarget.status, 0);
      assert.match(wrongTarget.stderr, /Expected an ARM64 Mach-O executable/);
      assert.equal(existsSync(marker), false);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

for (const platform of process.platform === 'darwin' ? ['macos', 'linux'] : ['linux']) {
  test(`archive packaging preserves ${platform} contents and discloses deferred target execution`, {
    skip: !((process.platform === 'darwin' && ['x64', 'arm64'].includes(process.arch)) ||
      (['win32', 'linux'].includes(process.platform) && process.arch === 'x64')),
  }, () => checkPackaging(platform));
}

test('publication requires all three exact-build signed artifacts and matching hashes', () => {
  const root = mkdtempSync(join(tmpdir(), 'rayfin-assemble-'));
  const source = { commit: 'a'.repeat(40), buildId: '123', version: '0.2.9' };
  const expected = { commit: source.commit, buildId: source.buildId, tag: 'v0.2.9' };
  const hash = data => createHash('sha256').update(data).digest('hex');
  const records = {};
  function record(platform, value) {
    writeFileSync(join(root, `${platform}-signed`, `${platform}-signing.json`), JSON.stringify(value));
  }
  try {
    for (const [platform, target] of Object.entries({ windows: 'x86_64-pc-windows-msvc', ...targets })) {
      const directory = join(root, `${platform}-signed`);
      mkdirSync(directory);
      const archive = `rayfin-local-0.2.9-${target}${platform === 'windows' ? '.zip' : '.tar.gz'}`;
      writeFileSync(join(directory, archive), platform);
      const metadata = { ...source, target, signed: true, archiveFile: archive, archiveSha256: hash(platform),
        signingKeyCode: { windows: 'CP-231522', linux: 'CP-450779-Pgp', macos: 'CP-401337-Apple' }[platform] };
      if (platform === 'linux') {
        writeFileSync(join(directory, `${archive}.sig`), 'signature');
        writeFileSync(join(directory, 'microsoft.asc'), 'public-key-fixture');
        writeFileSync(join(directory, 'install.sh'), 'installer-fixture');
        Object.assign(metadata, { signatureSha256: hash('signature'), fingerprint: linuxFingerprint });
      } else if (platform === 'macos') Object.assign(metadata, { notarized: true, teamId: 'UBF8T346G9' });
      else writeFileSync(join(directory, 'install.ps1'), 'installer-fixture');
      if (platform !== 'windows') metadata.nativeExecutionTested = false;
      records[platform] = metadata;
      record(platform, metadata);
    }
    assemble(root, join(root, 'release'), expected);
    assert.equal(readFileSync(join(root, 'release', 'SHA256SUMS.txt'), 'utf8').trim().split('\n').length, 4);
    assert.equal(existsSync(join(root, 'release', records.macos.archiveFile)), true);
    for (const [field, value] of [['commit', 'b'.repeat(40)], ['version', '0.2.10'],
      ['signed', false], ['notarized', false], ['teamId', 'WRONGTEAM0'],
      ['nativeExecutionTested', undefined], ['archiveSha256', '0'.repeat(64)]]) {
      record('macos', { ...records.macos, [field]: value });
      assert.throws(() => assemble(root, join(root, `bad-${field}`), expected), field);
    }
    record('macos', records.macos);
    rmSync(join(root, 'linux-signed', `${records.linux.archiveFile}.sig`));
    assert.throws(() => assemble(root, join(root, 'missing-signature'), expected));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
