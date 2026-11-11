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
