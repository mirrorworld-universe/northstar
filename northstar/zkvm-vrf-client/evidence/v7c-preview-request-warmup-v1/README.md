# Actual v7c preview request warmup

**One real candidate proof; performance target missed. Not the final three-case window.**

The generation client reported SDK verification of the retained wrapped proof. A local artifact-consistency audit matched every public byte with the independently executed CPU result and canonical native request output. This audit is **not independent cryptographic verification**. No complete fresh candidate Portal resolution or human acceptance is established by this package.

Measured on the selected **g5.xlarge / NVIDIA A10G** (23,028 MiB VRAM, driver 595.91.07):

- Process-cold prove + final Groth16 wrapping: **309.924 s**. The <=120 s target is **false**.
- Separate SDK verification: **0.499 s**; sum **310.423 s**.
- Generation-run CPU preflight: **6.057 s**, 122,290,302 cycles. Separate independent CPU execution: **6.154 s**.
- Client initialization **0.844 s**, setup **18.366 s**, total client wall time **336.531 s**. No resource peaks were measured here.

This proves the preview-session **request** witness. It does not establish fulfillment/callback proof timing, a harder try-count proof, three distinct prepared cases, or whole-resolution <=130,000 CU. Warm/cache labels follow the retained client definition: first proof in a new client after setup and CPU execution, with existing circuits. Do not reinterpret this as empty-cache provisioning or silently replace the slow result with a later optimized guest.

## Identity and contents

- Guest ELF SHA-256: `23543b749b4a7c3f35b580695392768d30e6f2ef99e7d13b4f8fcb7bd4412b34`, RV64IM flags 0.
- Genuine SDK program key: `0x001a26a674f911be746ffa5ea2dbdd412b0ee692bd34ebf2562b0b3bd25bd311`.
- Typed policy: `c627c521e8bea57fdebaf2e611a3c56b047e0ae18e3e2e1a586f64d86a8a4d0d`, one step, admission age 900.
- Deployment-profile file SHA-256: `024ba21c70e2d7f05b9833b4df373812231fe124e27a3a266b17c6c8827cdfce`. This is not the typed policy digest.
- Actual v5 client binary SHA-256: `81c63c9f6c8deb5e1f8a40145e553b47179576c76d7d782d024ba588c171f507`. The later release-cache/ISA/panic guards were not the measured binary.
- Actual frozen v5 client overlay SHA-256: `458729a1cb2188e3156428755c783b0a60ff4820cd11e8fab4d0da9725fb8867`; source concatenation hash and lock identity are in `component-audit.json`.

SP1 **SDK 6.8.0** uses the retained **v6.1.0 circuit family** here. Those are different version identities, not evidence of an SDK downgrade. The eight canonical fields retain kind3/version1 domain separation. See `profile.json` for the source/library/program/runtime identities and explicit development exclusions.

The small package contains the real SDK proof, 356-byte Portal envelope, 256-byte public values, SDK verifying-key/setup artifacts, deployment profile, path-free original measurements, hardware identity and native public bytes. The `unverified` file is the generation client's pre-verification copy; its bytes equal the final SDK proof, but its filename alone never grants verification. `component-audit.json` reports artifact checks only and retains the missed target.

Large ELF/witness/capture bytes and raw logs remain in retained private evidence, identified by hashes; they are not embedded here. Reproduction requires those exact artifacts or regeneration from the reviewed sources/canonical producer. The source base is [34d6b3e2](https://github.com/mirrorworld-universe/northstar/tree/34d6b3e2ff744c680d76ccdbfbf9aa2f3dba0d52), and the immutable library is [d1b8c3ff](https://github.com/mirrorworld-universe/vrf-coordinator-program-library/tree/d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd). No private keys, raw logs or machine-local paths are included.

No production rollout, hosted/delegated handoff, cross-layer settlement, fixture-owner approval, Baixiong acceptance or Jonas reproduction is claimed.
