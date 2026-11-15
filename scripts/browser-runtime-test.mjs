#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { readFileSync, rmSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { acquireLock } from './e2e-lane.mjs';

const lock = process.env.PM_E2E_LOCK || '/tmp/puppet-master-e2e-heavy.lock';
const target = resolve(process.env.CARGO_TARGET_DIR || 'target');
const env = { ...process.env, PM_BROWSER_RUNTIME_DIR: join(target, 'browser-runtime') };
function run(program, args) {
  const result = spawnSync(program, args, { stdio: 'inherit', env });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status || 1);
}
try {
  acquireLock(lock);
  process.once('exit', () => {
    if (readFileSync(lock, 'utf8').trim() === String(process.pid)) rmSync(lock);
  });
  run(process.env.CARGO || 'cargo', ['build', '--profile', 'e2e', '--bin', 'pm']);
  run(join(target, 'e2e/pm'), ['browser', 'install']);
  run(process.execPath, ['--test', 'crates/pm-daemon/browser-runtime/service.test.mjs']);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
