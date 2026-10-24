# Experimental bounded VRF replay workspace

This workspace is separate from the accepted replay guest and key. It contains:

- `northstar-zkvm-vrf-program`: compilation qualification only. It always rejects
  after execution and commits no public values. Never use its ELF as an accepted
  candidate proof route.
- `northstar-zkvm-vrf-candidate`: opt-in `candidate` feature. Its build requires
  `NORTHSTAR_VRF_PROFILE_SHA256`, a reviewed nonzero profile digest compiled into
  its own ELF/key. It decodes the canonical envelope, invokes the actual pinned
  interpreter, checks complete recomputed effects and checkpoint binding, then
  commits the unchanged eight 32-byte public fields in kind-3/version-1 order.
- `shared`: native/guest orchestration that always instantiates the real engine.
- `script`: native replay of the same envelope/shared implementation, writing
  exactly 256 expected public bytes to a fresh file. It cannot generate a proof.
  Use the separately owned `northstar/zkvm-vrf-client` for actual SP1 execution,
  key generation and distinct-case local GPU measurements; no duplicate SDK
  client or second GPU submission path is embedded here.

The engine uses Agave's interpreter, CPI/account machinery and syscall
implementations, not a supplied execution trace. Build feasibility, real native
replay, SP1 execution, wrapped proof timing and real Portal verification remain
separate validation gates. No performance or production acceptance is implied.

The separately built, workspace-only official SHA-256 implementation variant is
specified in [ACCELERATION-V1.md](ACCELERATION-V1.md). Regular and diagnostic
builds use distinct ELF/key records; neither silently replaces the baseline.
