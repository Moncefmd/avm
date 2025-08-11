package internal

import (
	"os"
	"path/filepath"
	"strings"
)

// GetActiveVersion reads the symlink to determine the active version.
func GetActiveVersion(homeDir string) (string, error) {
	symlinkPath := filepath.Join(homeDir, ".avm", "bin", "argocd")
	target, err := os.Readlink(symlinkPath)
	if err != nil {
		if os.IsNotExist(err) {
			return "", nil // No active version, not an error
		}
		return "", err
	}

	// The target will be something like /Users/user/.avm/versions/v2.3.4/argocd
	// We want to extract the version part.
	parts := strings.Split(filepath.ToSlash(target), "/")
	if len(parts) >= 3 && parts[len(parts)-3] == "versions" {
		return parts[len(parts)-2], nil
	}
	return "", nil // Could not determine version from path
}
