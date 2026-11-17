import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';

import { CACHE_DIR, rayfinLocalBin } from './paths.ts';

/*
 * The PIDs of the `rayfin-local serve` processes this harness started, so a run that was killed
 * before its teardown (which Windows' job objects normally cover) can't leave one behind.
 */

const FILE = join(CACHE_DIR, 'pids.json');

function read(): Record<string, string> {
  try {
    return JSON.parse(readFileSync(FILE, 'utf8')) as Record<string, string>;
  } catch {
    return {};
  }
}

function write(pids: Record<string, string>): void {
  mkdirSync(CACHE_DIR, { recursive: true });
  writeFileSync(FILE, JSON.stringify(pids, null, 2));
}

export function rememberPid(pid: number, label: string): void {
  write({ ...read(), [pid]: label });
}

export function forgetPid(pid: number): void {
  const pids = read();
  delete pids[pid];
  write(pids);
}

/**
 * Kills the process tree of `pid` at once (Windows: `taskkill /T /F`). Elsewhere: its process
 * group and every descendant, since serve starts Vite and the function host in process groups of
 * their own, which a killed serve can't end.
 */
export function killTree(pid: number): void {
  if (process.platform === 'win32') {
    spawnSync('taskkill', ['/T', '/F', '/PID', String(pid)], { stdio: 'ignore', windowsHide: true });
    return;
  }
  const descendants = descendantsOf(pid);
  for (const target of [-pid, pid, ...descendants.flatMap((child) => [-child, child])]) {
    try {
      process.kill(target, 'SIGKILL');
    } catch {
      // Already gone, or not a group leader.
    }
  }
}

/** Every process `pid` started, and those they started (Unix, from `ps`). */
function descendantsOf(pid: number): number[] {
  const out = spawnSync('ps', ['-A', '-o', 'pid=,ppid='], { encoding: 'utf8' }).stdout ?? '';
  const children = new Map<number, number[]>();
  for (const line of out.split('\n')) {
    const [child, parent] = line.trim().split(/\s+/).map(Number);
    if (!Number.isInteger(child) || !Number.isInteger(parent)) continue;
    children.set(parent, [...(children.get(parent) ?? []), child]);
  }
  const found: number[] = [];
  const queue = [pid];
  while (queue.length) {
    for (const child of children.get(queue.pop()!) ?? []) {
      if (child === pid || found.includes(child)) continue;
      found.push(child);
      queue.push(child);
    }
  }
  return found;
}

/** The executable name of a running process, or null when there is none with that PID. */
function imageName(pid: number): string | null {
  if (process.platform === 'win32') {
    const out = spawnSync('tasklist', ['/FI', `PID eq ${pid}`, '/FO', 'CSV', '/NH'], {
      encoding: 'utf8',
      windowsHide: true,
    }).stdout;
    const match = /^"([^"]+)","(\d+)"/m.exec(out ?? '');
    return match && Number(match[2]) === pid ? match[1] : null;
  }
  const out = spawnSync('ps', ['-p', String(pid), '-o', 'comm='], { encoding: 'utf8' }).stdout;
  return out?.trim() || null;
}

/** Kills what an earlier, interrupted run left running; returns what it killed. */
export function killLeftovers(): string[] {
  const killed: string[] = [];
  const image = basename(rayfinLocalBin()).toLowerCase();
  for (const [pid, label] of Object.entries(read())) {
    const name = imageName(Number(pid));
    if (name && basename(name).toLowerCase() === image) {
      killTree(Number(pid));
      killed.push(`${label} (pid ${pid})`);
    }
  }
  write({});
  return killed;
}
