//go:build icicle

package sp1

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"io"
	"os"
	"runtime/debug"
	"testing"

	"github.com/consensys/gnark-crypto/ecc"
	constraint_bn254 "github.com/consensys/gnark/constraint/bn254"
	"github.com/consensys/gnark/frontend"
	"github.com/consensys/gnark/frontend/cs/r1cs"
)

type readerCircuit struct {
	X        frontend.Variable
	Expected frontend.Variable `gnark:",public"`
}

func (c *readerCircuit) Define(api frontend.API) error {
	api.AssertIsEqual(api.Mul(c.X, c.X), c.Expected)
	return nil
}

func encodedReaderCircuit(t *testing.T) []byte {
	t.Helper()
	circuit, err := frontend.Compile(ecc.BN254.ScalarField(), r1cs.NewBuilder, &readerCircuit{})
	if err != nil {
		t.Fatal(err)
	}
	// Release builds otherwise omit frontend diagnostic metadata.
	circuit.AttachDebugInfo(circuit.NewDebugInfo("reader invariant"), []int{0})
	var encoded bytes.Buffer
	if _, err := circuit.WriteTo(&encoded); err != nil {
		t.Fatal(err)
	}
	return encoded.Bytes()
}

func TestR1CSReaderPreservesAllConstraintBytesAndChecks(t *testing.T) {
	encoded := encodedReaderCircuit(t)
	original := bytes.Clone(encoded)
	baseline := &constraint_bn254.R1CS{}
	if _, err := baseline.ReadFrom(bytes.NewReader(encoded)); err != nil {
		t.Fatal(err)
	}
	if len(baseline.DebugInfo) == 0 || len(baseline.MDebug) == 0 {
		t.Fatal("fixture must contain diagnostic metadata")
	}
	reader, err := withoutR1CSDiagnostics(bytes.NewReader(encoded), int64(len(encoded)))
	if err != nil {
		t.Fatal(err)
	}
	filtered := &constraint_bn254.R1CS{}
	if _, err := filtered.ReadFrom(reader); err != nil {
		t.Fatal(err)
	}
	if filtered.DebugInfo != nil || filtered.MDebug != nil {
		t.Fatal("diagnostics were materialized")
	}
	baseline.DebugInfo = nil
	baseline.MDebug = nil
	var want, got bytes.Buffer
	if _, err := baseline.WriteTo(&want); err != nil {
		t.Fatal(err)
	}
	if _, err := filtered.WriteTo(&got); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(want.Bytes(), got.Bytes()) {
		t.Fatal("non-diagnostic constraint-system bytes changed")
	}
	if !bytes.Equal(encoded, original) {
		t.Fatal("source bytes changed")
	}
	for _, circuit := range []*constraint_bn254.R1CS{baseline, filtered} {
		for _, expected := range []int{9, 10} {
			witness, err := frontend.NewWitness(&readerCircuit{X: 3, Expected: expected}, ecc.BN254.ScalarField())
			if err != nil {
				t.Fatal(err)
			}
			_, err = circuit.Solve(witness)
			if (err == nil) != (expected == 9) {
				t.Fatalf("expected=%d constraint check changed: %v", expected, err)
			}
		}
	}
}

func TestR1CSReaderPinnedArtifact(t *testing.T) {
	path := os.Getenv("SP1_R1CS_TEST_ARTIFACT")
	if path == "" {
		t.Skip("requires the retained canonical outer circuit artifact")
	}
	digest := func(filter bool) [32]byte {
		file, err := os.Open(path)
		if err != nil {
			t.Fatal(err)
		}
		defer file.Close()
		var reader io.Reader = file
		if filter {
			stat, err := file.Stat()
			if err != nil {
				t.Fatal(err)
			}
			reader, err = withoutR1CSDiagnostics(file, stat.Size())
			if err != nil {
				t.Fatal(err)
			}
		}
		circuit := &constraint_bn254.R1CS{}
		if _, err := circuit.ReadFrom(reader); err != nil {
			t.Fatal(err)
		}
		if circuit.GetNbConstraints() != 15972262 {
			t.Fatal("unexpected outer constraint count")
		}
		circuit.DebugInfo = nil
		circuit.MDebug = nil
		debug.FreeOSMemory()
		hash := sha256.New()
		if _, err := circuit.WriteTo(hash); err != nil {
			t.Fatal(err)
		}
		return [32]byte(hash.Sum(nil))
	}
	baseline := digest(false)
	debug.FreeOSMemory()
	filtered := digest(true)
	if baseline != filtered {
		t.Fatal("canonical outer circuit bytes differ after diagnostic removal")
	}
	t.Logf("identical outer circuit serialization after removing two diagnostic fields: %x", baseline)
}

func TestR1CSReaderRejectsTruncatedOrDifferentLayout(t *testing.T) {
	encoded := encodedReaderCircuit(t)
	for _, size := range []int{0, 31, 63, len(encoded) - 1} {
		if _, err := withoutR1CSDiagnostics(bytes.NewReader(encoded[:size]), int64(size)); err == nil {
			t.Fatalf("accepted truncated size %d", size)
		}
	}
	for _, offset := range []int{0, 8, 16, 32, 40, 48, 56} {
		changed := bytes.Clone(encoded)
		binary.LittleEndian.PutUint64(changed[offset:offset+8], ^uint64(0))
		if _, err := withoutR1CSDiagnostics(bytes.NewReader(changed), int64(len(changed))); err == nil {
			t.Fatalf("accepted changed header at %d", offset)
		}
	}
	if _, err := withoutR1CSDiagnostics(bytes.NewReader(encoded[:63]), int64(len(encoded))); err == nil {
		t.Fatal("accepted short underlying reader")
	}
}

func TestR1CSReaderRejectsMissingDiagnosticsAndInvalidBody(t *testing.T) {
	// A small pinned-layout envelope isolates the CBOR field-map guard.
	for _, body := range [][]byte{{0xa0}, {0xff}, {0x80}} {
		encoded := make([]byte, 64)
		binary.LittleEndian.PutUint64(encoded[:8], uint64(32+len(body)))
		binary.LittleEndian.PutUint64(encoded[16:24], 14)
		binary.LittleEndian.PutUint64(encoded[56:64], uint64(len(body)))
		encoded = append(encoded, body...)
		reader, err := withoutR1CSDiagnostics(bytes.NewReader(encoded), int64(len(encoded)))
		if err == nil {
			_, _ = io.ReadAll(reader)
			t.Fatal("accepted missing diagnostics or invalid CBOR")
		}
	}
}
