package sp1

import (
	"errors"
	"reflect"
	"strings"
	"testing"
)

func TestPreloadRejectsEmptyDirectoryBeforeLoading(t *testing.T) {
	called := false
	load := func(string) { called = true }
	if err := preloadGroth16("", "all", load, load); err == nil || called {
		t.Fatalf("empty directory: error=%v called=%v", err, called)
	}
}

func TestPreloadRetainsDirectoryAndLoadOrder(t *testing.T) {
	for _, component := range []string{"", "all"} {
		var calls []string
		err := preloadGroth16("artifacts", component, func(dir string) {
			calls = append(calls, "circuit:"+dir)
		}, func(dir string) {
			calls = append(calls, "key:"+dir)
		})
		if err != nil || !reflect.DeepEqual(calls, []string{"circuit:artifacts", "key:artifacts"}) {
			t.Fatalf("initialization: error=%v calls=%v", err, calls)
		}
	}
}

func TestPreloadKeyOnly(t *testing.T) {
	var calls []string
	err := preloadGroth16("artifacts", "key", func(string) {
		t.Fatal("key-only initialization loaded the circuit")
	}, func(dir string) {
		calls = append(calls, dir)
	})
	if err != nil || !reflect.DeepEqual(calls, []string{"artifacts"}) {
		t.Fatalf("key initialization: error=%v calls=%v", err, calls)
	}
}

func TestPreloadRejectsUnknownComponent(t *testing.T) {
	called := false
	load := func(string) { called = true }
	if err := preloadGroth16("artifacts", "unknown", load, load); err == nil || called {
		t.Fatalf("unknown component: error=%v called=%v", err, called)
	}
}

func TestPreloadReturnsLoaderFailure(t *testing.T) {
	for _, stage := range []string{"circuit", "key"} {
		t.Run(stage, func(t *testing.T) {
			var calls []string
			load := func(name string) func(string) {
				return func(string) {
					calls = append(calls, name)
					if name == stage {
						panic(errors.New(name + " read failed"))
					}
				}
			}
			err := preloadGroth16("artifacts", "all", load("circuit"), load("key"))
			if err == nil || !strings.Contains(err.Error(), stage+" read failed") {
				t.Fatalf("loader failure was not retained: %v", err)
			}
			want := []string{"circuit"}
			if stage == "key" {
				want = append(want, "key")
			}
			if !reflect.DeepEqual(calls, want) {
				t.Fatalf("loading continued after failure: %v", calls)
			}
		})
	}
}
