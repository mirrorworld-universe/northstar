// Sonic: An existing rustup binary does not imply the checkout's pinned compiler is installed.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { expect, test } from 'bun:test';
const action = Bun.YAML.parse(await Bun.file(new URL('../actions/ensure-rust-toolchain/action.yml', import.meta.url)).text());
const script = action.runs.steps[0].run;

test('existing rustup installs and verifies the exact checkout toolchain without resetting homes', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'northstar-pinned-rust-'));
  try {
    fs.mkdirSync(path.join(root, '.cargo/bin'), { recursive: true });
    fs.mkdirSync(path.join(root, '.rustup'));
    const marker = path.join(root, '.rustup/retained-marker');
    fs.writeFileSync(marker, 'retained');
    fs.writeFileSync(path.join(root, 'rust-toolchain.toml'), '[toolchain]\nchannel = "1.99.1"\n');
    const log = path.join(root, 'calls');
    fs.writeFileSync(path.join(root, '.cargo/bin/rustup'), '#!/usr/bin/env bash\nprintf "%s\\n" "$*" >> "$RUSTUP_CALLS"\n', { mode: 0o700 });
    const result = Bun.spawnSync(['bash', '-c', script], { cwd: root, env: { ...process.env, HOME: root,
      GITHUB_PATH: path.join(root, 'github-path'), RUSTUP_CALLS: log } });
    expect(result.exitCode).toBe(0);
    const calls = fs.existsSync(log) ? fs.readFileSync(log, 'utf8') : '';
    expect(calls).toContain('toolchain install 1.99.1 --profile minimal --no-self-update');
    expect(calls).toContain('run 1.99.1 rustc --version');
    expect(fs.readFileSync(marker, 'utf8')).toBe('retained');
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test('native preflight is master-only and contains no activation or private state operations', async () => {
  const workflow = Bun.YAML.parse(await Bun.file(new URL('../workflows/native-build-preflight.yml', import.meta.url)).text());
  expect(workflow.permissions).toEqual({ contents: 'read' });
  const job = workflow.jobs.prepare;
  expect(job.if).toContain("github.ref == 'refs/heads/master'");
  expect(job['runs-on']).toEqual(['devnet']);
  expect(job.steps.map(step => step.uses).filter(Boolean)).toEqual(['actions/checkout@v4', './.github/actions/ensure-rust-toolchain']);
  const commands = job.steps.map(step => step.run ?? '').join('\n');
  expect(commands).toContain('cargo build-sbf --version');
  expect(commands).not.toMatch(/sudo|systemctl|program deploy|continuity_devnet\.py|funder|journal/);
  expect(Bun.spawnSync(['bash', '-n'], { stdin: Buffer.from(commands) }).exitCode).toBe(0);
});

test('toolchain setup never deletes existing cargo/rustup homes or changes the default', () => {
  expect(script).not.toMatch(/rm\s+-rf|rustup\s+default/);
  expect(Bun.spawnSync(['bash', '-n'], { stdin: Buffer.from(script) }).exitCode).toBe(0);
});
