import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { assemble } from '../.github/scripts/assemble-release.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const source = join(root, 'npm/rayfin-local');
const client = join(root, 'npm/rayfin-local-runtime');
const placements = {
  windows: 'bin/win32-x64/rayfin-local.exe',
  macos: 'bin/darwin-arm64/rayfin-local',
  linux: 'bin/linux-x64/rayfin-local',
};
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const readJson = (file) => JSON.parse(readFileSync(file, 'utf8'));

function archiveEntry(archive, member) {
  // macOS ships BSD tar (ZIP support); GNU tar on Linux needs unzip for ZIPs.
  const zipOnLinux = process.platform === 'linux' && archive.endsWith('.zip');
  const tar = process.platform === 'win32' ? join(process.env.SystemRoot, 'System32/tar.exe') : 'tar';
  return execFileSync(zipOnLinux ? 'unzip' : tar, [zipOnLinux ? '-p' : '-xOf', archive, member],
    { maxBuffer: 64 * 1024 * 1024 });
}

export function extractExecutable(archive, { binarySha256, version, target }, destination) {
  const member = `rayfin-local-${version}-${target}/${basename(destination)}`;
  const binary = archiveEntry(archive, member);
  assert.equal(sha256(binary), binarySha256, `${target} executable hash mismatch`);
  mkdirSync(dirname(destination), { recursive: true });
  writeFileSync(destination, binary);
  chmodSync(destination, 0o755);
}

function packRelease(artifacts, out, { commit, buildId, tag }) {
  assert.notEqual(process.platform, 'win32', 'Pack on a POSIX host to preserve Unix executable modes.');
  assert.match(commit ?? '', /^[0-9a-f]{40}$/);
  assert.match(buildId ?? '', /^\d+$/);
  const version = readFileSync(join(root, 'Cargo.toml'), 'utf8').match(/\[workspace\.package\][\s\S]*?\nversion = "([^"]+)"/)?.[1];
  assert.match(version ?? '', /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/, 'The npm preview lane requires a stable Cargo version.');
  assert.equal(tag, `v${version}`, 'The signed release tag must match the Cargo version.');

  const temporary = mkdtempSync(join(tmpdir(), 'rayfin-npm-release-'));
  try {
    const release = join(temporary, 'signed');
    const pkg = join(temporary, 'package');
    assemble(artifacts, release, { commit, buildId, tag });
    mkdirSync(pkg);
    const binaries = {};
    for (const [platform, location] of Object.entries(placements)) {
      const metadata = readJson(join(release, `${platform}-signing.json`));
      extractExecutable(join(release, metadata.archiveFile), metadata, join(pkg, location));
      binaries[location] = { target: metadata.target, sha256: metadata.binarySha256 };
    }
    for (const file of ['server.cjs', 'server.mjs', 'server.d.cts', 'server.d.mts', 'events.d.cts', 'binary.mjs', 'binary.d.cts', 'binary.d.mts']) {
      copyFileSync(join(client, file), join(pkg, file));
    }
    for (const file of ['binary.cjs', 'README.md']) copyFileSync(join(source, file), join(pkg, file));
    copyFileSync(join(root, 'LICENSE'), join(pkg, 'LICENSE'));
    const manifest = readJson(join(source, 'package.json'));
    delete manifest.private;
    writeFileSync(join(pkg, 'package.json'), `${JSON.stringify({ ...manifest, version }, null, 2)}\n`);
    writeFileSync(join(pkg, 'native-build.json'), `${JSON.stringify({ commit, buildId, version, binaries }, null, 2)}\n`);

    mkdirSync(out, { recursive: true });
    const [packed] = JSON.parse(execFileSync('npm', [
      'pack', '--json', '--ignore-scripts', '--pack-destination', out,
    ], { cwd: pkg, encoding: 'utf8' }));
    assert.equal(packed.name, manifest.name);
    assert.equal(packed.version, version);
    const tarball = join(out, packed.filename);
    for (const [location, { sha256: expected }] of Object.entries(binaries)) {
      assert.equal(sha256(archiveEntry(tarball, `package/${location}`)), expected, `${location} changed during npm pack`);
      assert.ok(packed.files.find(({ path }) => path === location)?.mode & 0o111, `${location} lost execute permission`);
    }
    console.log(`Packed ${packed.id} from signed build ${buildId}: ${tarball}`);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const { values } = parseArgs({ options: { artifacts: { type: 'string' }, out: { type: 'string' } } });
  if (!values.artifacts || !values.out) throw new Error('Supply --artifacts <pipeline workspace> and --out <npm artifact directory>.');
  packRelease(resolve(values.artifacts), resolve(values.out), {
    commit: process.env.BUILD_SOURCEVERSION, buildId: process.env.BUILD_BUILDID, tag: process.env.RELEASE_TAG,
  });
}
