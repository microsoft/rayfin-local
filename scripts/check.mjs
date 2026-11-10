#!/usr/bin/env node
// The one way to check a change, locally and in CI (.github/workflows/ci.yml runs these steps).
//
//   node scripts/check.mjs              setup, lint, rust, js and e2e: everything CI checks
//   node scripts/check.mjs rust js      only those steps (after setup, see below)
//   node scripts/check.mjs --help       what each step does
//
// Steps:
//   setup  installs the test fixtures' and harnesses' npm packages (again only when a
//          package-lock.json changed), builds the functions fixture and, for e2e or when asked for
//          by name, downloads Playwright's Chromium (not when PLAYWRIGHT_CHANNEL picks a browser).
//   lint   cargo fmt --check, and clippy with warnings as errors.
//   rust   cargo test --workspace in strict mode (RAYFIN_LOCAL_TEST_STRICT=1: a test whose Node,
//          fixture packages or SDK are missing fails instead of skipping), and the SDK auth test.
//   js     node --test js/test (the helper scripts rayfin-local embeds).
//   e2e    the browser suites (tests/e2e: the fixture and static projects) against a real
//          `rayfin-local serve`, and the sign-in portal test (crates/rl-auth/tests/portal_e2e.rs).
//
// rust, js and e2e run setup first, which takes seconds once done; --no-setup skips it.
// Needs Rust 1.96 or later, Node.js 22 or later and npm.

import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const at = (...parts) => path.join(ROOT, ...parts);

/** Folders whose packages (package-lock.json) the tests need. */
const NPM_DIRS = [
  'tests/fixtures/apps/basic',
  'tests/fixtures/apps/basic/rayfin/functions',
  'tests/fixtures/apps/connectors',
  'tests/fixtures/functions/basic',
  'crates/rl-storage/tests/sdk',
  'tests/e2e',
  'tests/e2e/portal',
];

/** Where the Rust tests that run the real SDK find it: the basic fixture has every package. */
const SDK_NODE_MODULES = at('tests/fixtures/apps/basic/node_modules');

const STEPS = {
  setup: { run: setup, about: 'npm ci the test fixtures and harnesses, build the functions fixture, get Chromium for e2e' },
  lint: { run: lint, about: 'cargo fmt --check, cargo clippy -D warnings' },
  rust: { run: rust, needsSetup: true, about: 'cargo test --workspace, strict (an SDK test cannot skip)' },
  js: { run: js, needsSetup: true, about: 'node --test js/test' },
  e2e: { run: e2e, needsSetup: true, about: 'Playwright fixture and static suites, the sign-in portal test' },
};
const ORDER = Object.keys(STEPS);

function main(argv) {
  const options = argv.filter((arg) => arg.startsWith('-'));
  const names = argv.filter((arg) => !arg.startsWith('-'));
  if (options.some((option) => ['--help', '-h', '--list'].includes(option))) {
    console.log('usage: node scripts/check.mjs [--no-setup] [step...]\n');
    for (const [name, step] of Object.entries(STEPS)) console.log(`  ${name.padEnd(6)} ${step.about}`);
    console.log('\nWithout a step: all of them.');
    return;
  }
  for (const option of options) {
    if (option !== '--no-setup') fail(`unknown option ${option} (see --help)`);
  }
  for (const name of names) {
    if (!(name in STEPS) && name !== 'all') fail(`unknown step "${name}" (see --help)`);
  }
  const steps = new Set(names.length === 0 || names.includes('all') ? ORDER : names);
  if ([...steps].some((name) => STEPS[name].needsSetup)) steps.add('setup');
  if (options.includes('--no-setup')) steps.delete('setup');

  const started = Date.now();
  const run = ORDER.filter((name) => steps.has(name));
  const context = { browser: steps.has('e2e') || names.includes('setup') };
  for (const name of run) {
    banner(`${name}: ${STEPS[name].about}`);
    const stepStarted = Date.now();
    STEPS[name].run(context);
    console.log(`[check] ${name} passed in ${seconds(stepStarted)}`);
  }
  banner(`passed: ${run.join(', ')} in ${seconds(started)}`);
}

function setup({ browser }) {
  for (const dir of NPM_DIRS) npmCi(dir);
  // Incremental: quick when nothing changed. js/test/function-host.test.mjs runs the build.
  exec('npm', ['run', 'build'], { cwd: at('tests/fixtures/functions/basic') });
  if (!browser) return;
  if (process.env.PLAYWRIGHT_CHANNEL) {
    console.log(`[check] PLAYWRIGHT_CHANNEL=${process.env.PLAYWRIGHT_CHANNEL}: not downloading Chromium`);
  } else {
    // --with-deps also installs the system libraries Chromium needs, which Linux CI machines lack.
    const withDeps = process.env.CI && process.platform === 'linux' ? ['--with-deps'] : [];
    exec(process.execPath, [playwrightCli(), 'install', ...withDeps, 'chromium'], { cwd: at('tests/e2e') });
  }
}

function lint() {
  exec('cargo', ['fmt', '--all', '--check']);
  exec('cargo', ['clippy', '--workspace', '--all-targets', '--', '-D', 'warnings']);
}

function rust() {
  exec('cargo', ['test', '--workspace'], { env: strictEnv() });
  exec('cargo', ['test', '-p', 'rl-auth', '--test', 'sdk', '--', '--ignored'], { env: strictEnv() });
}

function js() {
  exec(process.execPath, ['--test', 'js/test/']);
}

function e2e() {
  exec(process.execPath, [playwrightCli(), 'test', '--project=fixture', '--project=static'], {
    cwd: at('tests/e2e'),
  });
  exec('cargo', ['test', '-p', 'rl-auth', '--test', 'portal_e2e', '--', '--ignored'], { env: strictEnv() });
}

function strictEnv() {
  return {
    RAYFIN_LOCAL_TEST_STRICT: '1',
    RAYFIN_SDK_NODE_MODULES: process.env.RAYFIN_SDK_NODE_MODULES || SDK_NODE_MODULES,
  };
}

function playwrightCli() {
  return at('tests/e2e/node_modules/@playwright/test/cli.js');
}

const STAMP = '.rayfin-local-check';

/** `npm ci` in `dir`, unless its node_modules came from the same package-lock.json. */
function npmCi(dir) {
  const hash = createHash('sha256').update(readFileSync(at(dir, 'package-lock.json'))).digest('hex');
  const stamp = at(dir, 'node_modules', STAMP);
  if (existsSync(stamp) && readFileSync(stamp, 'utf8').trim() === hash) {
    console.log(`[check] ${dir}: packages up to date`);
    return;
  }
  // --ignore-scripts: none of these packages needs an install script (esbuild and Rollup ship their
  // binaries as optional per-platform packages).
  exec('npm', ['ci', '--ignore-scripts', '--no-audit', '--no-fund'], { cwd: at(dir) });
  writeFileSync(stamp, `${hash}\n`);
}

/** Runs `command` with its output shown; exits with its status when it fails. */
function exec(command, args, { cwd = ROOT, env = {} } = {}) {
  const shown = [command === process.execPath ? 'node' : command, ...args].join(' ');
  console.log(`[check] ${path.relative(ROOT, cwd) || '.'}> ${shown}`);
  const options = { cwd, env: { ...process.env, ...env }, stdio: 'inherit' };
  // npm is a .cmd script on Windows, which only a shell runs; these arguments need no quoting.
  const result =
    process.platform === 'win32' && command === 'npm'
      ? spawnSync(process.env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', `npm ${args.join(' ')}`], options)
      : spawnSync(command, args, options);
  if (result.error) fail(`${shown}: ${result.error.message}`);
  if (result.status !== 0) fail(`${shown} failed (${result.status ?? result.signal})`, result.status || 1);
}

function banner(text) {
  console.log(`\n[check] ==== ${text}`);
}

function seconds(since) {
  return `${Math.round((Date.now() - since) / 1000)}s`;
}

function fail(message, code = 2) {
  console.error(`[check] FAILED: ${message}`);
  process.exit(code);
}

main(process.argv.slice(2));
