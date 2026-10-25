//go:build !icicle

package sp1

import "fmt"

func PreloadGroth16(dataDir string) error {
	return fmt.Errorf("overlapping Groth16 initialization requires the ICICLE backend")
}
