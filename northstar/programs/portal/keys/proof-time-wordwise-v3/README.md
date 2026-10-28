# Proof-time development key — not installed

Separate key for the streamed-binding and exact aligned-word comparison guest. It does not replace the existing Portal key or establish production acceptance.

- SP1 SDK: 6.8.0; outer circuit: v6.1.0.
- ELF SHA-256: `e95546332b8e6116b2f752131991823b33946ddbc80d8de1886b745670fbb0e4`.
- Program key: `0x003d5a315b22935c59616b8a5f2e6d1b6c9584c1738d38dc245f45cb925ddf32`.
- Typed policy, eight public fields, one checkpoint step and 149-byte session encoding are unchanged.

The receipt comes from real CPU SDK setup; CUDA setup reproduced the key. The regular guest passed all 17 execution controls and both valid rollback witnesses. Real Groth16 proofs SDK-verified with exact public bytes. Best recorded process-cold old-request diagnostic: **142137 ms** prove+wrap and **490 ms** SDK verification on an A10G. This is neither fresh-session qualification nor a 120-second success.

[`bounded_vrf_key.py`](../../tools/bounded_vrf_key.py) generated the manifest and constants. The receipt establishes artifact consistency, not authenticated execution or independent human review. `source.json` pins the reviewed source bytes. The ELF is excluded because it contains compiler build-tree strings.

Configured-SBF tests, three fresh configured-session proofs, a new 100-cycle hard window, complete Production resolutions, independent reproduction, L40S equivalence and hosted acceptance remain pending. **Do not install this as a production release key.**
