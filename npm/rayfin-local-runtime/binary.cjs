'use strict';

const { artifact } = require('./resolve.cjs');
exports.binaryPath = artifact(process.platform === 'win32' ? 'rayfin-local.exe' : 'rayfin-local');
