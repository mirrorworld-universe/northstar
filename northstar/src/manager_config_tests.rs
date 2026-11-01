use super::*;

#[test]
fn regression_validator_checkpoint_namespace() {
    const MODE: &str = "NORTHSTAR_CONFIG_NAMESPACE_TEST";
    const DIRECTORY: &str = "NORTHSTAR_CHECKPOINT_PLAN_DIR";
    if let Some(mode) = std::env::var_os(MODE) {
        let config = ManagerConfig::for_validator(
            Pubkey::new_unique(),
            Arc::new(Keypair::new()),
            std::path::Path::new("/retained-ledger"),
        );
        let expected = if mode == "override" {
            PathBuf::from("/isolated-v2/checkpoints")
        } else {
            PathBuf::from("/retained-ledger/northstar-checkpoint-plans")
        };
        assert_eq!(config.checkpoint_plan_dir, Some(expected));
        return;
    }
    for mode in ["default", "override"] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "manager_config_tests::regression_validator_checkpoint_namespace",
                "--nocapture",
            ])
            .env(MODE, mode)
            .env_remove(DIRECTORY);
        if mode == "override" {
            command.env(DIRECTORY, "/isolated-v2/checkpoints");
        }
        assert!(
            command.status().unwrap().success(),
            "namespace mode: {mode}"
        );
    }
}
