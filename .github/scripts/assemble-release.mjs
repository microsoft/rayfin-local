import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { checkSource, targets, linuxFingerprint, appleTeam } from './unix-release.mjs';

const hash = path => createHash('sha256').update(readFileSync(path)).digest('hex');

export function assemble(workspace, destination, { commit, buildId, tag }) {
  const windowsDirectory = join(workspace, 'windows-signed');
  const windows = JSON.parse(readFileSync(join(windowsDirectory, 'windows-signing.json'), 'utf8'));
  const version = windows.version;
  assert.match(version, /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/);
  assert.equal(tag, `v${version}`);
  const platforms = { windows: 'x86_64-pc-windows-msvc', ...targets };
  mkdirSync(destination);
  const sums = [];
  for (const [platform, target] of Object.entries(platforms)) {
    const directory = join(workspace, `${platform}-signed`);
    const file = `${platform}-signing.json`;
    const metadata = JSON.parse(readFileSync(join(directory, file), 'utf8'));
    checkSource(metadata, { commit, buildId, version, target });
    assert.equal(metadata.signed, true, `${platform} is not signed`);
    assert.equal(metadata.signingKeyCode, {
      windows: 'CP-231522', linux: 'CP-450779-Pgp', macos: 'CP-401337-Apple',
    }[platform]);
    if (platform === 'macos') {
      assert.equal(metadata.notarized, true);
      assert.equal(metadata.teamId, appleTeam);
    }
    const archive = `rayfin-local-${version}-${target}${platform === 'windows' ? '.zip' : '.tar.gz'}`;
    assert.equal(metadata.archiveFile, archive);
    assert.equal(hash(join(directory, archive)), metadata.archiveSha256, `${platform} archive hash mismatch`);
    copyFileSync(join(directory, archive), join(destination, archive));
    sums.push(`${metadata.archiveSha256}  ${archive}`);
    copyFileSync(join(directory, file), join(destination, file));
    if (platform === 'linux') {
      assert.equal(metadata.fingerprint, linuxFingerprint);
      assert.equal(hash(join(directory, `${archive}.sig`)), metadata.signatureSha256);
      copyFileSync(join(directory, `${archive}.sig`), join(destination, `${archive}.sig`));
      sums.push(`${metadata.signatureSha256}  ${archive}.sig`);
      copyFileSync(join(directory, 'microsoft.asc'), join(destination, 'microsoft.asc'));
      copyFileSync(join(directory, 'install.sh'), join(destination, 'install.sh'));
    }
  }
  copyFileSync(join(windowsDirectory, 'install.ps1'), join(destination, 'install.ps1'));
  writeFileSync(join(destination, 'SHA256SUMS.txt'), `${sums.join('\n')}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  assemble(process.env.PIPELINE_WORKSPACE, join(process.env.PIPELINE_WORKSPACE, 'release'), {
    commit: process.env.BUILD_SOURCEVERSION, buildId: process.env.BUILD_BUILDID, tag: process.env.RELEASE_TAG,
  });
}
