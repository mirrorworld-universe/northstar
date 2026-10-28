package sp1

import "fmt"

func preloadGroth16(dataDir, component string, loadCircuit, loadKey func(string)) (err error) {
	if dataDir == "" {
		return fmt.Errorf("Groth16 artifact directory is required")
	}
	if component != "" && component != "all" && component != "key" {
		return fmt.Errorf("Groth16 initialization component must be all or key")
	}
	defer func() {
		if failure := recover(); failure != nil {
			err = fmt.Errorf("Groth16 initialization: %v", failure)
		}
	}()
	if component != "key" {
		loadCircuit(dataDir)
	}
	loadKey(dataDir)
	return nil
}
