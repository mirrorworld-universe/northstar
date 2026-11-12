// Sonic: Hosted activation must use the same guarded controller and publish no private state.
import { test, expect } from 'bun:test';

const workflow = Bun.YAML.parse(await Bun.file(new URL('../workflows/ci.yml', import.meta.url)).text());
const deployment = workflow.jobs.deploy;
const steps = deployment.steps;

test('native validation uses a pinned supported Ubuntu image', () => {
  for (const name of ['fmt', 'clippy', 'test', 'network-prover']) {
    expect(workflow.jobs[name]['runs-on']).toBe('ubuntu-24.04');
  }
});

test('L1 RPC profile is frozen before builds and never selects fresh activation', () => {
  const mode = steps.find(step => step.id === 'continuity_mode');
  expect(mode.env.L1_RPC_PROFILE).toBe('${{ vars.NORTHSTAR_L1_RPC_PROFILE }}');
  expect(mode.run).toContain('--l1-rpc-profile "$L1_RPC_PROFILE"');
  const selected = Bun.spawnSync(['python3', '-B', '.github/scripts/continuity_devnet.py', 'mode', '--l1-rpc-profile', 'full']);
  expect(selected.exitCode).toBe(0);
  expect(selected.stdout.toString().trim()).toBe('--upgrade-existing --l1-rpc-profile full');
  const mixed = Bun.spawnSync(['python3', '-B', '.github/scripts/continuity_devnet.py', 'mode',
    '--replace-context', 'ab'.repeat(32), '--l1-rpc-profile', 'full']);
  expect(mixed.exitCode).not.toBe(0);
});

test('fresh release mode is frozen before builds and remains separate from recovery', () => {
  const mode = steps.find(step => step.id === 'continuity_mode');
  expect(mode).toBeDefined();
  expect(steps.indexOf(mode)).toBeLessThan(steps.findIndex(step => step.id === 'continuity_stage'));
  expect(mode.env.FRESH_CONTEXT).toBe('${{ vars.NORTHSTAR_FRESH_CONTEXT }}');
  expect(mode.env.L1_RECOVERY_SLOT).toBe('${{ vars.NORTHSTAR_L1_RECOVERY_SLOT }}');
  expect(mode.run).toContain('continuity_devnet.py mode');
  expect(mode.run).toContain('--replace-context "$FRESH_CONTEXT"');
  const activation = steps.find(step => step.id === 'continuity_activate');
  expect(activation.env.NATIVE_MODE_ARGS).toBe('${{ steps.continuity_mode.outputs.arguments }}');
  expect(activation.run).toContain('"${activation_args[@]}"');
  expect(activation.env).not.toHaveProperty('L1_RECOVERY_SLOT');
  expect(steps.find(step => step.name === 'Notify Slack').run).toContain('fresh-stack)');
  const guards = workflow.jobs.test.steps.find(step => step.name === 'Test continuity-v2 activation guards').run;
  expect(guards).toContain('test-fresh-devnet.py');
});

test('release-only deployment stages both inputs before isolated activation', () => {
  expect(deployment.if).toContain("github.ref == 'refs/heads/release'");
  expect(deployment.if).toContain("github.event_name == 'push'");
  const stage = steps.findIndex(step => step.id === 'continuity_stage');
  const sdk = steps.findIndex(step => step.id === 'continuity_sdk');
  const activation = steps.findIndex(step => step.id === 'continuity_activate');
  expect(stage).toBeGreaterThanOrEqual(0);
  expect(sdk).toBeGreaterThanOrEqual(0);
  expect(activation).toBeGreaterThan(stage);
  expect(activation).toBeGreaterThan(sdk);
  expect(steps[activation].env).toEqual({
    NATIVE_STAGE: '${{ steps.continuity_stage.outputs.directory }}',
    CONTINUITY_SDK: '${{ steps.continuity_sdk.outputs.directory }}',
    NATIVE_MODE_ARGS: '${{ steps.continuity_mode.outputs.arguments }}',
  });
  expect(steps[activation].run).toContain('continuity_devnet.py run');
  expect(steps[activation].run).toContain('--revision "$GITHUB_SHA"');
  expect(steps[activation].run).toContain('read -r -a activation_args');
  const defaultMode = Bun.spawnSync(['python3', '-B', '.github/scripts/continuity_devnet.py', 'mode']);
  expect(defaultMode.exitCode).toBe(0);
  expect(defaultMode.stdout.toString().trim()).toBe('--upgrade-existing');
  expect(steps[activation]['timeout-minutes']).toBe(120);
  expect(steps.find(step => step.uses === 'actions/setup-node@v4').with['node-version']).toBe('24.21.0');
  const testSteps = workflow.jobs.test.steps;
  expect(testSteps.find(step => step.uses === 'actions/setup-node@v4').with['node-version']).toBe('24.21.0');
  const sdkCheck = testSteps.find(step => step.name === 'Test pinned public continuity SDK install').run;
  expect(sdkCheck).toContain('prepare-continuity-sdk.sh');
  expect(sdkCheck).toContain('NORTHSTAR_TEST_SDK_CHECKOUT="$SDK" node --test .github/scripts/native-upgrade-snapshot.test.mjs');
});

test('failed L1 recovery is explicitly selected, scoped and regression tested', () => {
  const activation = steps.find(step => step.id === 'continuity_activate');
  const mode = steps.find(step => step.id === 'continuity_mode');
  expect(mode.run).toContain('--recovery-slot "$L1_RECOVERY_SLOT"');
  expect(activation.run).toContain('"${activation_args[@]}"');
  const selected = Bun.spawnSync(['python3', '-B', '.github/scripts/continuity_devnet.py', 'mode', '--recovery-slot', '519264000']);
  expect(selected.exitCode).toBe(0);
  expect(selected.stdout.toString().trim()).toBe('--upgrade-existing --recover-failed-l1-slot 519264000');
  const mixed = Bun.spawnSync(['python3', '-B', '.github/scripts/continuity_devnet.py', 'mode', '--recovery-slot', '519264000', '--replace-context', 'ab'.repeat(32)]);
  expect(mixed.exitCode).not.toBe(0);
  const guards = workflow.jobs.test.steps.find(step => step.name === 'Test continuity-v2 activation guards').run;
  expect(guards).toContain('test-native-l1-recovery.py');
  const compatibility = workflow.jobs.test.steps.find(step => step.name === 'Test active devnet feature and implicit account locks').run;
  expect(compatibility).toContain('cargo test --locked -p agave-feature-set --features agave-unstable-api --lib');
  expect(compatibility).toContain('cargo test --locked -p solana-svm --features agave-unstable-api --lib account_loader::tests');
  expect(compatibility).toContain('cargo test --locked -p solana-core --lib banking_stage::transaction_scheduler');
  const notification = steps.find(step => step.name === 'Notify Slack');
  expect(notification.run).toContain('native-l1-recovery)');
  expect(notification.run).toContain('Prior L1 data and all ER journals were preserved.');
});

test('pending upload diagnostics remain master-only and never invoke activation', async () => {
  const diagnostic = Bun.YAML.parse(await Bun.file(new URL('../workflows/native-runtime-diagnostics.yml', import.meta.url)).text());
  const job = diagnostic.jobs.inspect;
  expect(job.if).toContain("github.ref == 'refs/heads/master'");
  const collect = job.steps.find(step => step.id === 'diagnostics');
  expect(collect.env.PENDING_REVISION).toBe('${{ inputs.pending_revision }}');
  expect(collect.run).toContain('--pending-revision "$PENDING_REVISION"');
  expect(collect.run).not.toMatch(/systemctl|program deploy|continuity_devnet\.py|install/);
  expect(job.steps.map(step => step.uses).filter(Boolean)).toEqual(['actions/checkout@v4', 'actions/upload-artifact@v4']);
});

test('retained upload reconciliation is explicit master-only and never starts native services', async () => {
  const uploadWorkflow = Bun.YAML.parse(await Bun.file(new URL('../workflows/continuity-upload-resume.yml', import.meta.url)).text());
  const job = uploadWorkflow.jobs.reconcile;
  expect(job.if).toContain("github.ref == 'refs/heads/master'");
  expect(job['runs-on']).toBe('devnet');
  const step = job.steps.find(value => value.id === 'reconcile');
  expect(step.env.TARGET_REVISION).toBe('${{ inputs.revision }}');
  expect(step.env.REPLACE_CONTEXT).toBe('${{ inputs.replace_context }}');
  expect(step.env.FAILED_RECORD_SHA256).toBe('${{ inputs.failed_record_sha256 }}');
  expect(step.run).toContain('options+=(--failed-record-sha256 "$FAILED_RECORD_SHA256")');
  expect(step.run).toContain('continuity_upload_resume.py');
  expect(step.run).not.toMatch(/systemctl|build-continuity|continuity_devnet\.py|--stage|fund/);
  expect(job.steps.at(-1).with.path).toBe('${{ steps.reconcile.outputs.report_path }}');
  expect(workflow.jobs.test.steps.map(value => value.run ?? '').join('\n')).toContain('test-continuity-upload-resume.py');
});

test('upload reconciliation exposes the installed Solana toolkit before controller invocation', async () => {
  const { mkdtemp, mkdir, writeFile, rm } = await import('node:fs/promises');
  const { tmpdir } = await import('node:os');
  const home = await mkdtemp(`${tmpdir()}/upload-tool-path-`);
  try {
    const binaryDirectory = `${home}/.local/share/solana/install/active_release/bin`;
    await mkdir(binaryDirectory, { recursive: true });
    await writeFile(`${binaryDirectory}/solana-keygen`, '#!/bin/sh\nexit 0\n', { mode: 0o700 });
    const uploadWorkflow = Bun.YAML.parse(await Bun.file(new URL('../workflows/continuity-upload-resume.yml', import.meta.url)).text());
    const run = uploadWorkflow.jobs.reconcile.steps.find(step => step.id === 'reconcile').run;
    const setup = run.split('\n').find(line => line.trim().startsWith('export PATH=')) ?? ':';
    const result = Bun.spawnSync([Bun.which('bash'), '-c', `${setup}; command -v solana-keygen`], {
      env: { ...process.env, HOME: home, PATH: '/usr/bin:/bin' },
    });
    expect(result.exitCode).toBe(0);
    expect(result.stdout.toString().trim()).toBe(`${binaryDirectory}/solana-keygen`);
  } finally {
    await rm(home, { recursive: true, force: true });
  }
});

test('prepared fresh continuation binds retained plan and publishes only qualified evidence', async () => {
  const continuation = Bun.YAML.parse(await Bun.file(new URL('../workflows/fresh-prepared-continuation.yml', import.meta.url)).text());
  const job = continuation.jobs.activate;
  expect(job.if).toContain("github.ref == 'refs/heads/master'");
  const activate = job.steps.find(step => step.id === 'activate');
  expect(activate.env.PLAN_SHA256).toBe('${{ inputs.plan_sha256 }}');
  expect(activate.env.FAILED_CONTINUATION_SHA256).toBe('${{ inputs.failed_continuation_sha256 }}');
  expect(activate.run).toContain('options+=(--failed-continuation-sha256 "$FAILED_CONTINUATION_SHA256")');
  expect(activate.run).toContain('fresh_prepared_continuation.py');
  expect(activate.run).toContain('export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"');
  expect(activate.run).not.toMatch(/systemctl|build-continuity|--stage|variable set/);
  expect(job.steps.at(-1).if).toContain("steps.activate.conclusion == 'success'");
  expect(job.steps.at(-1).with.path).toBe('${{ steps.activate.outputs.report_path }}');
  const checks = workflow.jobs.test.steps.map(step => step.run ?? '').join('\n');
  expect(checks).toContain('test-fresh-prepared-continuation.py');
  expect(checks).toContain('test-continuity-rpc.py');
});

test('poststart reconciliation is master-only and requires the exact read-only review', async () => {
  const value = Bun.YAML.parse(await Bun.file(new URL('../workflows/fresh-poststart-reconciliation.yml', import.meta.url)).text());
  const job = value.jobs.reconcile;
  expect(job.if).toContain("github.ref == 'refs/heads/master'");
  const step = job.steps.find(step => step.id === 'reconcile');
  expect(step.env.REVIEW_SHA256).toBe('${{ inputs.review_sha256 }}');
  expect(step.run).toContain('fresh_poststart_reconciliation.py');
  expect(step.run).toContain('export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"');
  expect(step.run).not.toMatch(/build-continuity|continuity_upload_resume|--stage|variable set|systemctl/);
  expect(job.steps.at(-1).if).toContain("steps.reconcile.conclusion == 'success'");
  expect(job.steps.at(-1).with.path).toBe('${{ steps.reconcile.outputs.report_path }}');
  expect(workflow.jobs.test.steps.map(step => step.run ?? '').join('\n')).toContain('test-fresh-poststart-reconciliation.py');
});

test('artifact upload selects only the verified public report', () => {
  const uploads = steps.filter(step => step.uses?.startsWith('actions/upload-artifact@'));
  expect(uploads).toHaveLength(1);
  expect(uploads[0].with.path).toBe('${{ steps.continuity_activate.outputs.report_path }}');
  expect(uploads[0].with['if-no-files-found']).toBe('error');
});

test('inline deployment commands do not replace retained keys or invoke legacy consumers', () => {
  const scripts = steps.map(step => step.run ?? '').join('\n');
  for (const old of ['rm -f "$KEYPAIR"', 'sudo sed -i', 'sudo systemctl restart', 'portal open-session', 'devnet-e2e-smoke.sh', 'devnet-spl-e2e-smoke.sh']) {
    expect(scripts).not.toContain(old);
  }
  expect(scripts).toContain('oracle remains stopped');
});

test('upgrade scope is separate from fresh protocol qualification in Slack', () => {
  const notification = steps.find(step => step.name === 'Notify Slack');
  expect(notification.env.QUALIFICATION_KIND).toBe('${{ steps.continuity_activate.outputs.qualification_kind }}');
  expect(notification.run).toContain('case "$QUALIFICATION_KIND"');
  expect(notification.run).toContain('native-upgrade)');
  expect(notification.run).toContain('This is not a fresh SOL/SPL payout');
  expect(notification.run).toContain('No programs were deployed.');
  const guards = workflow.jobs.test.steps.find(step => step.name === 'Test continuity-v2 activation guards').run;
  expect(guards).toContain('test-native-upgrade.py');
  expect(guards).toContain('native-upgrade-snapshot.test.mjs');
});

test('all workflow shell blocks parse without executing them', () => {
  for (const job of Object.values(workflow.jobs)) {
    for (const step of job.steps) {
      if (step.run) expect(Bun.spawnSync(['bash', '-n'], { stdin: Buffer.from(step.run) }).exitCode).toBe(0);
    }
  }
});

test('retained local evidence explicitly excludes hosted and proof qualification', async () => {
  const evidence = await Bun.file(new URL('../evidence/continuity-v2-activation-local.json', import.meta.url)).json();
  expect(evidence.result).toBe('passed');
  expect(evidence.loaded_program_hashes_match).toBe(true);
  expect(evidence.assertions.validator_stopped).toBe(true);
  for (const qualified of Object.values(evidence.qualification_limits)) expect(qualified).toBe(false);
  expect(evidence.genesis_feature_deactivation).toHaveLength(1);
  expect(JSON.stringify(evidence)).not.toMatch(/\/home\/|\/tmp\/|\/nix\/|\.pi\//);
});
