// Sonic: Keep package setup independent of rotating mirror lists without relaxing archive trust.
import { test, expect } from 'bun:test';
const workflow = Bun.YAML.parse(await Bun.file(new URL('../workflows/ci.yml', import.meta.url)).text());
const actionUrl = new URL('../actions/install-linux-dependencies/action.yml', import.meta.url);

test('hosted native and proof jobs use one bounded signed package installer', () => {
  for (const [name, packages] of Object.entries({
    clippy: 'protobuf-compiler libprotobuf-dev libudev-dev libclang-dev',
    test: 'protobuf-compiler libprotobuf-dev libudev-dev libclang-dev',
    'network-prover': 'libssl-dev pkg-config clang protobuf-compiler libprotobuf-dev',
  })) {
    const step = workflow.jobs[name].steps.find(step => step.uses === './.github/actions/install-linux-dependencies');
    expect(step?.with?.packages).toBe(packages);
  }
});

test('isolated HTTPS Ubuntu sources retain signature verification and bounded retries', async () => {
  const action = Bun.YAML.parse(await Bun.file(actionUrl).text());
  expect(action.runs.using).toBe('composite');
  const script = action.runs.steps[0].run;
  expect(script).toContain('https://archive.ubuntu.com/ubuntu');
  expect(script).toContain('https://mirrors.edge.kernel.org/ubuntu');
  expect(script).toContain('/usr/share/keyrings/ubuntu-archive-keyring.gpg');
  expect(script).toContain('Dir::Etc::sourceparts=');
  expect(script).toContain('Acquire::Retries=2');
  expect(script).toContain('APT::Update::Error-Mode=any');
  expect(script).not.toMatch(/trusted=yes|allow-unauthenticated|AllowInsecure|Verify-Peer=false/);
  expect(Bun.spawnSync(['bash', '-n'], { stdin: Buffer.from(script) }).exitCode).toBe(0);
});
