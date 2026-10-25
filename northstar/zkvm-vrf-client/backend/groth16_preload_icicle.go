//go:build icicle

package sp1

import "os"

// PreloadGroth16 uses the same mutex-protected loaders as ProveGroth16. The caller
// starts it only after entering prove_with_mode, never during client setup.
func PreloadGroth16(dataDir string) error {
	return preloadGroth16(dataDir, os.Getenv("SP1_GROTH16_OVERLAP_COMPONENT"), loadGroth16Circuit, loadGroth16ProvingKey)
}
