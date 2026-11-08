// Sonic: Manual host inspection must stay read-only and publish only its sanitized report.
import { test, expect } from 'bun:test';

const workflow = Bun.YAML.parse(await Bun.file(new URL('../workflows/native-runtime-diagnostics.yml', import.meta.url)).text());

test('diagnostics are manual, master-only, and use the existing devnet runner', () => {
  expect(Object.keys(workflow.on)).toEqual(['workflow_dispatch']);
  expect(workflow.permissions).toEqual({ contents: 'read' });
  const job = workflow.jobs.inspect;
  expect(job.if).toContain("github.ref == 'refs/heads/master'");
  expect(job['runs-on']).toEqual(['devnet']);
  expect(job['timeout-minutes']).toBe(5);
  expect(job.steps[0].with['persist-credentials']).toBe(false);
});

test('only the fixed read-only helper runs; only its report is uploaded', () => {
  const steps = workflow.jobs.inspect.steps;
  const script = steps.find(step => step.id === 'diagnostics').run;
  expect(script).toContain('sudo -n python3 -B .github/scripts/native_runtime_diagnostics.py');
  for (const mutation of ['systemctl start', 'systemctl stop', 'systemctl restart', 'continuity_devnet.py run', 'rm ', 'install ']) {
    expect(script).not.toContain(mutation);
  }
  expect(Bun.spawnSync(['bash', '-n'], { stdin: Buffer.from(script) }).exitCode).toBe(0);
  const uploads = steps.filter(step => step.uses?.startsWith('actions/upload-artifact@'));
  expect(uploads).toHaveLength(1);
  expect(uploads[0].with.path).toBe('${{ steps.diagnostics.outputs.report_path }}');
  expect(uploads[0].with['if-no-files-found']).toBe('error');
});
