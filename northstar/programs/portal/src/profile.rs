#[inline(always)]
pub(crate) fn mark(label: &'static str) {
    #[cfg(all(feature = "zk-verifier-profile", target_os = "solana"))]
    unsafe {
        pinocchio::syscalls::sol_log_(label.as_ptr(), label.len() as u64);
        pinocchio::syscalls::sol_log_compute_units_();
    }
    #[cfg(not(all(feature = "zk-verifier-profile", target_os = "solana")))]
    let _ = label;
}
