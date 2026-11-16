import assert from 'node:assert/strict';
import { test } from 'node:test';
import { alphaManifest } from './set-alpha-version.mjs';

test('only changes the workspace version, using the official build number', () => {
  const source = '[workspace.package]\nversion = "0.2.8"\nedition = "2024"\n\n[workspace.dependencies]\nexample = { version = "1" }\n';
  const result = alphaManifest(source, '21422341');
  assert.equal(result.version, '0.2.8-alpha.21422341');
  assert.equal(result.manifest, source.replace('version = "0.2.8"', 'version = "0.2.8-alpha.21422341"'));
  assert.equal(alphaManifest(result.manifest, '21422342').version, '0.2.8-alpha.21422342');
});

test('rejects missing workspace metadata and invalid build identifiers', () => {
  assert.throws(() => alphaManifest('[package]\nversion = "0.2.8"\n', '123'));
  for (const id of ['', '0', '-1', '123/other', '$(Build.BuildId)']) {
    assert.throws(() => alphaManifest('[workspace.package]\nversion = "0.2.8"\n', id));
  }
});
