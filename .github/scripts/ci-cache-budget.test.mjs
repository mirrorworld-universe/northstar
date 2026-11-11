// Sonic: Large dependency restores must not exhaust ephemeral runner storage before tests start.
import { expect, test } from "bun:test";

const workflow = Bun.YAML.parse(await Bun.file(new URL("../workflows/ci.yml", import.meta.url)).text());

test.each(["clippy", "test"])("%s prepares hosted-runner disk before restoring Rust artifacts", (job) => {
  const steps = workflow.jobs[job].steps;
  const cleanup = steps.findIndex((step) => step.uses === "./.github/actions/prepare-runner-disk");
  const cache = steps.findIndex((step) => step.uses === "Swatinem/rust-cache@v2");
  expect(cleanup).toBeGreaterThanOrEqual(0);
  expect(cache).toBeGreaterThan(cleanup);
  expect(steps[cache].with["prefix-key"]).toBe("native-no-debug-ubuntu24-v2");
});

test("native CI uses bounded debug artifacts rather than restoring the old full-debug profile", () => {
  expect(String(workflow.env.CARGO_PROFILE_DEV_DEBUG)).toBe("0");
  expect(String(workflow.env.CARGO_PROFILE_TEST_DEBUG)).toBe("0");
});

test("disk preparation is restricted to hosted runners and unused preinstalled SDKs", async () => {
  const action = Bun.YAML.parse(await Bun.file(new URL("../actions/prepare-runner-disk/action.yml", import.meta.url)).text());
  expect(action.runs.using).toBe("composite");
  expect(action.runs.steps).toHaveLength(1);
  for (const step of action.runs.steps) {
    expect(step.if).toBe("runner.environment == 'github-hosted'");
    expect(step.shell).toBe("bash");
    const removal = step.run.split("\n").filter((line) => line.startsWith("sudo rm"));
    expect(removal).toEqual(["sudo rm -rf /usr/local/lib/android /usr/share/dotnet /opt/ghc"]);
    expect(Bun.spawnSync(["bash", "-n"], { stdin: Buffer.from(step.run) }).exitCode).toBe(0);
  }
});
