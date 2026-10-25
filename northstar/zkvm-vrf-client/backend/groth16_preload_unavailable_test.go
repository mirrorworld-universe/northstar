//go:build !icicle

package sp1

import (
	"strings"
	"testing"
)

func TestPreloadRequiresIcicle(t *testing.T) {
	if err := PreloadGroth16("artifacts"); err == nil || !strings.Contains(err.Error(), "ICICLE") {
		t.Fatalf("unsupported backend did not reject initialization: %v", err)
	}
}
