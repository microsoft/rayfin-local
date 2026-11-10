import { randomBytes } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { FIXTURE_DIR, RESULTS_DIR } from './paths.ts';
import { ensureNodeModules, fakeToken, FixtureCopy, newUser, readState } from './project.ts';
import { ServeProcess } from './serve.ts';
import type { RunningSuite } from './suites.ts';

/**
 * A copy of the fixture app (`FixtureCopy`, in `test-results/static-app`) with
 * `assetAccess: protected`, served with `--static --no-functions`: built once and served from the
 * backend's origin behind the hosting gate, with nobody signed in up front.
 */
export async function startStaticSuite(): Promise<RunningSuite> {
  await ensureNodeModules(FIXTURE_DIR, 'fixture');
  const copy = new FixtureCopy(join(RESULTS_DIR, 'static-app'));
  let serve: ServeProcess | undefined;
  const stop = async () => {
    try {
      await serve?.stop();
    } finally {
      copy.remove();
    }
  };
  try {
    const yml = join(copy.dir, 'rayfin', 'rayfin.yml');
    const config = readFileSync(yml, 'utf8');
    const protectedConfig = config.replace(/^(\s*assetAccess:\s*)public\s*$/m, '$1protected');
    if (protectedConfig === config) throw new Error(`${yml} has no \`assetAccess: public\` to change.`);
    writeFileSync(yml, protectedConfig);
    writeFileSync(join(copy.dir, 'rayfin-local.toml'), '[auth]\nsign_in_as = "none"\n');

    const tokens = { RAYFIN_LOCAL_TOKEN_POWERBI: fakeToken('powerbi') };
    const controlToken = randomBytes(24).toString('hex');
    serve = await ServeProcess.start({
      projectDir: copy.dir,
      label: 'static',
      env: { ...tokens, RAYFIN_LOCAL_CONTROL_TOKEN: controlToken },
      frontend: false,
      args: ['--static', '--no-functions'],
    });
    return {
      info: {
        name: 'static',
        serve: serve.info,
        state: readState(copy.dir),
        user: newUser(),
        tokens,
        controlToken,
      },
      stop,
    };
  } catch (error) {
    await stop();
    throw error;
  }
}
