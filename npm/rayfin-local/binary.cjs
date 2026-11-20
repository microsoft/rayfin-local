'use strict';

const fs = require('node:fs');
const path = require('node:path');

const { platform, arch } = process;
const target = `${platform}-${arch}`;
if (!['win32-x64', 'darwin-arm64', 'linux-x64'].includes(target)
  || (platform === 'linux' && !process.report.getReport().header.glibcVersionRuntime)) {
  throw new Error(`Unsupported Rayfin Local platform: ${platform}/${arch}. Use Windows x64, macOS Apple Silicon, or Linux x64 with glibc.`);
}

const binaryPath = path.join(__dirname, 'bin', target, platform === 'win32' ? 'rayfin-local.exe' : 'rayfin-local');
if (!fs.statSync(binaryPath, { throwIfNoEntry: false })?.isFile()) {
  throw new Error('The bundled Rayfin Local executable is missing. Reinstall @microsoft/rayfin-local.');
}
exports.binaryPath = binaryPath;
