package main

import "C"
import "github.com/succinctlabs/sp1-recursion-gnark/sp1"

//export PreloadGroth16Bn254
func PreloadGroth16Bn254(dataDir *C.char) *C.char {
	if err := sp1.PreloadGroth16(C.GoString(dataDir)); err != nil {
		return C.CString(err.Error())
	}
	return nil
}
