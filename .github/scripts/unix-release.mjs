import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, mkdirSync, copyFileSync, mkdtempSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { workspaceVersion } from './main-release.mjs';

export const linuxFingerprint = 'BC528686B50D79E339D3721CEB3E94ADBE1229CF';
export const appleTeam = 'UBF8T346G9';
export const targets = {
  linux: 'x86_64-unknown-linux-gnu',
  macos: 'aarch64-apple-darwin',
};

export function verifyLinuxElf(binary) {
  assert.ok(binary.length >= 64, 'Linux executable must contain a complete ELF64 header');
  assert.deepEqual([...binary.subarray(0, 7)], [0x7f, 0x45, 0x4c, 0x46, 2, 1, 1],
    'Expected a little-endian ELF64 executable');
  assert.ok([2, 3].includes(binary.readUInt16LE(16)), 'Expected an ELF executable or PIE');
  assert.equal(binary.readUInt16LE(18), 62, 'Expected an x86-64 Linux executable');
  assert.equal(binary.readUInt32LE(20), 1, 'Unexpected ELF version');
}

export function verifyGpgStatus(status) {
  assert.doesNotMatch(status, /\[GNUPG:\] (?:BADSIG|ERRSIG|EXPSIG|EXPKEYSIG|REVKEYSIG|KEYEXPIRED|SIGEXPIRED|FAILURE)\b/,
    'Linux signature or signing key is invalid, expired or revoked');
  const signatures = status.split(/\r?\n/).filter(line => line.startsWith('[GNUPG:] VALIDSIG '));
  assert.equal(signatures.length, 1, 'Expected exactly one valid Linux archive signature');
  assert.equal(signatures[0].split(' ')[2], linuxFingerprint, 'Linux signature must use the pinned Microsoft key');
  assert.ok(['8', '9', '10'].includes(signatures[0].split(' ')[9]), 'Linux signature must use SHA-256 or stronger');
}

export function verifyAppleIdentity(details) {
  assert.match(details, new RegExp(`^TeamIdentifier=${appleTeam}$`, 'm'), 'Unexpected Apple Team ID');
  assert.match(details, /^Authority=Developer ID Application: Microsoft Corporation \(UBF8T346G9\)$/m);
  assert.match(details, /^Timestamp=(?!none$).+$/m, 'Apple signature must have a timestamp');
  assert.match(details, /^.*flags=.*\bruntime\b.*$/m, 'Apple signature must enable hardened runtime');
}

export function checkSource(metadata, { commit, buildId, version, target }) {
  assert.equal(metadata.commit, commit, 'Artifact source commit mismatch');
  assert.equal(metadata.buildId, buildId, 'Artifact build ID mismatch');
  assert.equal(metadata.version, version, 'Artifact version mismatch');
  assert.equal(metadata.target, target, 'Artifact target mismatch');
}

function run(command, args) {
  const result = spawnSync(command, args, { encoding: 'utf8' });
  if (result.error) throw result.error;
  assert.equal(result.status, 0, `${command} failed:\n${result.stdout}\n${result.stderr}`);
  return { stdout: result.stdout.trim(), stderr: result.stderr.trim() };
}

const hash = path => createHash('sha256').update(readFileSync(path)).digest('hex');

function linuxArchive(action, archive, directory, name) {
  run(process.platform === 'win32' ? 'python' : 'python3', ['-c', `
import pathlib, sys, tarfile
action, archive, directory, name = sys.argv[1:]
files = {name + "/" + file for file in ("rayfin-local", "README.md", "LICENSE")}
if action == "package":
    def permissions(info):
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        info.mode = 0o755 if info.isdir() or info.name == name + "/rayfin-local" else 0o644
        return info
    with tarfile.open(archive, "w:gz", format=tarfile.USTAR_FORMAT) as tar:
        tar.add(pathlib.Path(directory) / name, arcname=name, filter=permissions)
else:
    with tarfile.open(archive, "r:gz") as tar:
        members = tar.getmembers()
        assert len(members) == 4 and len({m.name for m in members}) == 4, "Unexpected archive entries"
        assert {m.name for m in members if m.isfile()} == files, "Unexpected archive files"
        assert all(m.isfile() or (m.isdir() and m.name == name) for m in members), "Unsafe archive entry"
        binary = tar.getmember(name + "/rayfin-local")
        assert binary.mode == 0o755, "Linux executable must retain mode 0755"
        tar.extractall(directory, filter="data")
`, action, archive, directory, name]);
}

function main() {
  const [action, platform] = process.argv.slice(2);
  assert.ok(['package', 'verify'].includes(action) && targets[platform], 'Expected package|verify and linux|macos');
  const target = targets[platform];
  if (platform === 'macos') assert.equal(process.platform, 'darwin');
  else assert.ok(['win32', 'linux', 'darwin'].includes(process.platform), 'Unsupported Linux cross-build/verification host');
  if (platform === 'macos') assert.ok(['x64', 'arm64'].includes(process.arch), 'Expected a macOS build/verification host');
  else assert.ok(['x64', 'arm64'].includes(process.arch), 'Expected an x64 or ARM64 cross-build/verification host');
  const version = workspaceVersion(readFileSync('Cargo.toml', 'utf8'));
  assert.match(version, /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/);
  const name = `rayfin-local-${version}-${target}`;
  const output = join(process.env.BUILD_ARTIFACTSTAGINGDIRECTORY, action === 'package' ? `unsigned-${platform}` : `${platform}-signed`);
  mkdirSync(output, { recursive: true });
  const expected = {
    commit: process.env.BUILD_SOURCEVERSION, buildId: process.env.BUILD_BUILDID, version, target,
    nativeExecutionTested: false,
  };
  assert.match(expected.commit, /^[a-f0-9]{40}$/);
  assert.match(expected.buildId, /^[1-9]\d*$/);
  const temp = mkdtempSync(join(process.env.AGENT_TEMPDIRECTORY, `rayfin-${platform}-`));
  const directory = join(temp, name);
  if (action === 'package') {
    mkdirSync(directory);
    const binary = join('target', target, 'release', 'rayfin-local');
    if (platform === 'macos') {
      assert.equal(run('lipo', ['-archs', binary]).stdout, 'arm64', 'Expected an ARM64 Mach-O executable');
    } else {
      verifyLinuxElf(readFileSync(binary));
    }
    for (const file of ['README.md', 'LICENSE']) copyFileSync(file, join(directory, file));
    copyFileSync(binary, join(directory, 'rayfin-local'));
    const archive = `${name}${platform === 'macos' ? '.zip' : '.tar.gz'}`;
    if (platform === 'macos') run('ditto', ['-c', '-k', '--keepParent', directory, join(output, archive)]);
    else linuxArchive('package', join(output, archive), temp, name);
    writeFileSync(join(output, `${platform}-source.json`), JSON.stringify({
      ...expected, archiveFile: archive, archiveSha256: hash(join(output, archive)), binarySha256: hash(binary),
    }, null, 2));
    return;
  }
  const input = join(process.env.PIPELINE_WORKSPACE, 'unix-signing');
  const source = JSON.parse(readFileSync(join(input, `${platform}-source.json`), 'utf8'));
  checkSource(source, expected);
  assert.equal(source.nativeExecutionTested, false, 'Cross-compiled artifacts must disclose deferred native execution');
  const originalArchive = `${name}${platform === 'macos' ? '.zip' : '.tar.gz'}`;
  assert.equal(source.archiveFile, originalArchive, 'Unexpected source archive name');
  const inputArchive = join(input, originalArchive);
  if (platform === 'linux') {
    assert.equal(hash(inputArchive), source.archiveSha256, 'Signing must not modify the Linux archive');
    const keyHome = join(temp, 'gnupg');
    mkdirSync(keyHome, { mode: 0o700 });
    const key = resolve('.azure-pipelines/keys/microsoft.asc');
    run(process.env.GPG || 'gpg', ['--batch', '--homedir', keyHome, '--import', key]);
    const status = run(process.env.GPG || 'gpg', ['--batch', '--homedir', keyHome, '--status-fd', '1',
      '--verify', `${inputArchive}.sig`, inputArchive]).stdout;
    verifyGpgStatus(status);
    copyFileSync(`${inputArchive}.sig`, join(output, `${name}.tar.gz.sig`));
    copyFileSync(key, join(output, 'microsoft.asc'));
    linuxArchive('extract', inputArchive, temp, name);
  } else {
    run('ditto', ['-x', '-k', inputArchive, temp]);
  }
  const binary = join(directory, 'rayfin-local');
  if (platform === 'macos') {
    assert.equal(run('lipo', ['-archs', binary]).stdout, 'arm64');
    run('codesign', ['--verify', '--strict', '--all-architectures', '--verbose=2', binary]);
    verifyAppleIdentity(run('codesign', ['--display', '--verbose=4', binary]).stderr);
    // `--type execute` only accepts app bundles; a bare CLI executable is assessed by its own signature.
    const assessment = run('spctl', ['--assess', '--type', 'open', '--context', 'context:primary-signature',
      '--verbose=4', binary]);
    assert.match(`${assessment.stdout}\n${assessment.stderr}`, /source=Notarized Developer ID/);
  } else {
    assert.equal(hash(binary), source.binarySha256, 'Linux executable changed after packaging');
    verifyLinuxElf(readFileSync(binary));
  }
  const archive = `${name}.tar.gz`;
  if (platform === 'macos') run('tar', ['-czf', join(output, archive), '-C', temp, name]);
  else copyFileSync(inputArchive, join(output, archive));
  copyFileSync('install.sh', join(output, 'install.sh'));
  const metadata = {
    ...expected, signed: true, binarySha256: hash(binary), archiveFile: archive,
    archiveSha256: hash(join(output, archive)),
    signingKeyCode: platform === 'linux' ? 'CP-450779-Pgp' : 'CP-401337-Apple',
    ...(platform === 'linux'
      ? { signatureSha256: hash(join(output, `${archive}.sig`)), fingerprint: linuxFingerprint }
      : { teamId: appleTeam, notarized: true }),
  };
  writeFileSync(join(output, `${platform}-signing.json`), JSON.stringify(metadata, null, 2));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
