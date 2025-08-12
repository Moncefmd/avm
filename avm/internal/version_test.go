package internal

import (
	"os"
	"path/filepath"
	"testing"
)

func TestGetActiveVersion(t *testing.T) {
	tempDir := t.TempDir()

	// Case 1: Active version is set
	versionsDir := filepath.Join(tempDir, ".avm", "versions", "v1.0.0")
	binDir := filepath.Join(tempDir, ".avm", "bin")
	os.MkdirAll(versionsDir, 0755)
	os.MkdirAll(binDir, 0755)

	binaryPath := filepath.Join(versionsDir, "argocd")
	os.WriteFile(binaryPath, []byte("dummy content"), 0755)

	symlinkPath := filepath.Join(binDir, "argocd")
	os.Symlink(binaryPath, symlinkPath)

	version, err := GetActiveVersion(tempDir)
	if err != nil {
		t.Fatalf("GetActiveVersion returned an error: %v", err)
	}
	if version != "v1.0.0" {
		t.Errorf("expected version v1.0.0, got %s", version)
	}

	// Case 2: No active version
	os.Remove(symlinkPath)
	version, err = GetActiveVersion(tempDir)
	if err != nil {
		t.Fatalf("GetActiveVersion returned an error: %v", err)
	}
	if version != "" {
		t.Errorf("expected empty version, got %s", version)
	}
}
