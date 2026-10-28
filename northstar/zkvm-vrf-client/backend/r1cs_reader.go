//go:build icicle

package sp1

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"io"

	"github.com/fxamacker/cbor/v2"
)

// withoutR1CSDiagnostics filters only the two diagnostic fields already discarded
// after loading. Binary constraint sections, coefficients and all other CBOR
// values remain byte-for-byte unchanged. Filtering happens inside the prove clock.
// The layout is pinned to gnark v0.14's constraint/{,bn254/}marshal.go.
func withoutR1CSDiagnostics(source io.ReaderAt, size int64) (io.Reader, error) {
	if size < 64 {
		return nil, fmt.Errorf("truncated R1CS header")
	}
	header := make([]byte, 64)
	if _, err := source.ReadAt(header, 0); err != nil {
		return nil, err
	}
	payload := uint64(size - 32)
	if binary.LittleEndian.Uint64(header[:8]) != payload ||
		binary.LittleEndian.Uint64(header[8:16]) != 0 ||
		binary.LittleEndian.Uint64(header[16:24]) != 14 {
		return nil, fmt.Errorf("R1CS filter requires the pinned gnark v0.14 layout")
	}
	prefix := uint64(32)
	for offset := 32; offset < 56; offset += 8 {
		length := binary.LittleEndian.Uint64(header[offset : offset+8])
		if length > payload-prefix {
			return nil, fmt.Errorf("R1CS binary section exceeds payload")
		}
		prefix += length
	}
	bodyLength := binary.LittleEndian.Uint64(header[56:64])
	if bodyLength > payload-prefix {
		return nil, fmt.Errorf("R1CS CBOR section exceeds payload")
	}
	bodyOffset := int64(32 + prefix)
	body := make([]byte, int(bodyLength))
	if _, err := source.ReadAt(body, bodyOffset); err != nil {
		return nil, err
	}
	mode, err := (cbor.DecOptions{
		MaxArrayElements: 2147483647,
		MaxMapPairs:      2147483647,
	}).DecMode()
	if err != nil {
		return nil, err
	}
	var fields map[string]cbor.RawMessage
	if err := mode.Unmarshal(body, &fields); err != nil {
		return nil, fmt.Errorf("decode R1CS field map: %w", err)
	}
	for _, name := range []string{"DebugInfo", "MDebug"} {
		if _, exists := fields[name]; !exists {
			return nil, fmt.Errorf("missing pinned R1CS field %s", name)
		}
		fields[name] = cbor.RawMessage{0xf6}
	}
	encoder, err := cbor.CoreDetEncOptions().EncMode()
	if err != nil {
		return nil, err
	}
	filtered, err := encoder.Marshal(fields)
	if err != nil {
		return nil, err
	}
	binary.LittleEndian.PutUint64(header[:8], payload-bodyLength+uint64(len(filtered)))
	binary.LittleEndian.PutUint64(header[56:64], uint64(len(filtered)))
	return io.MultiReader(
		bytes.NewReader(header),
		io.NewSectionReader(source, 64, int64(prefix-32)),
		bytes.NewReader(filtered),
		io.NewSectionReader(source, bodyOffset+int64(bodyLength), int64(payload-prefix-bodyLength)),
	), nil
}
