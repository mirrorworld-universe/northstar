# Token bridge verification

[Verification record and reproduction commands](../../portal/proofs/SETTLEMENT.md) cover the ER transfer, the L1 vault withdrawal counter and Portal's matching one-use authorization. The [bridge proof crate](token_accounting.rs) imports the [production decisions](../src/accounting.rs) by path. The [SBF integration tests](../tests/core/accounting.rs) separately exercise persistence and real Portal and token CPIs. They do not prove all SPL behavior or global vault solvency.
