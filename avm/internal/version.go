package internal

import (
	"fmt"
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

func UseVersion(version string) error {
	homeDir, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("error getting home directory: %w", err)
	}

	versionDir := filepath.Join(homeDir, ".avm", "versions", version)
	binaryPath := filepath.Join(versionDir, "argocd")

	if _, err := os.Stat(binaryPath); os.IsNotExist(err) {
		return fmt.Errorf("version %s is not installed. Please run 'avm install %s' first", version, version)
	}

	binDir := filepath.Join(homeDir, ".avm", "bin")
	if err := os.MkdirAll(binDir, 0755); err != nil {
		return fmt.Errorf("error creating bin directory: %w", err)
	}

	symlinkPath := filepath.Join(binDir, "argocd")

	// Remove existing symlink
	if _, err := os.Lstat(symlinkPath); err == nil {
		if err := os.Remove(symlinkPath); err != nil {
			return fmt.Errorf("error removing existing symlink: %w", err)
		}
	}

	if err := os.Symlink(binaryPath, symlinkPath); err != nil {
		return fmt.Errorf("error creating symlink: %w", err)
	}

	return nil
}
