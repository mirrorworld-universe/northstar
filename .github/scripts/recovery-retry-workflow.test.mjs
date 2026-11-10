// Sonic: Reconciliation must remain an explicit, bounded operation on retained state.
import { test, expect } from 'bun:test';

const workflow = Bun.YAML.parse(await Bun.file(new URL('../workflows/native-l1-recovery-retry.yml', import.meta.url)).text());

test('recovery reconciliation is manual, master-only and cannot cancel an active attempt', () => {
  expect(Object.keys(workflow.on)).toEqual(['workflow_dispatch']);
  expect(Object.keys(workflow.on.workflow_dispatch.inputs).sort()).toEqual(['failed_slot', 'recovery_revision', 'stopped_at']);
  expect(Object.values(workflow.on.workflow_dispatch.inputs).every(input => input.required)).toBe(true);
  expect(workflow.permissions).toEqual({ contents: 'read' });
  expect(workflow.concurrency['cancel-in-progress']).toBe(false);
  const job = workflow.jobs.reconcile;
  expect(job.if).toContain("github.ref == 'refs/heads/master'");
  expect(job['runs-on']).toEqual(['devnet']);
  expect(job['timeout-minutes']).toBe(90);
  expect(job.steps[0].with['fetch-depth']).toBe(0);
  expect(job.steps[0].with['persist-credentials']).toBe(false);
});

test('retry uses the pinned SDK and retained artifacts without deployment or reset commands', () => {
  const steps = workflow.jobs.reconcile.steps;
  expect(steps.find(step => step.id === 'sdk').run).toContain('prepare-continuity-sdk.sh');
  const operation = steps.find(step => step.id === 'retry');
  expect(operation.env.RECOVERY_REVISION).toBe('${{ inputs.recovery_revision }}');
  expect(operation.env.EXPECTED_STOP).toBe('${{ inputs.stopped_at }}');
  expect(operation.env.FAILED_SLOT).toBe('${{ inputs.failed_slot }}');
  expect(operation.run).toContain('--reconcile-failed-l1 --reconcile-stopped-at "$EXPECTED_STOP"');
  expect(operation.run).toContain('--upgrade-existing --recover-failed-l1-slot "$FAILED_SLOT"');
  for (const forbidden of ['--stage', '--funder-keypair', 'rm ', 'install ', 'solana-keygen', 'program deploy', 'systemctl start']) {
    expect(operation.run).not.toContain(forbidden);
  }
  expect(Bun.spawnSync(['bash', '-n'], { stdin: Buffer.from(operation.run) }).exitCode).toBe(0);
  const uploads = steps.filter(step => step.uses?.startsWith('actions/upload-artifact@'));
  expect(uploads).toHaveLength(1);
  expect(uploads[0].with.path).toBe('${{ steps.retry.outputs.report_path }}');
  expect(uploads[0].with['if-no-files-found']).toBe('error');
});
