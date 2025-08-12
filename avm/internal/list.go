package internal

import (
	"os"
	"path/filepath"
)

func GetInstalledVersions() ([]string, error) {
	homeDir, err := os.UserHomeDir()
	if err != nil {
		return nil, err
	}

	versionsDir := filepath.Join(homeDir, ".avm", "versions")
	if _, err := os.Stat(versionsDir); os.IsNotExist(err) {
		return []string{}, nil
	}

	entries, err := os.ReadDir(versionsDir)
	if err != nil {
		return nil, err
	}

	var installedVersions []string
	for _, entry := range entries {
		if entry.IsDir() {
			installedVersions = append(installedVersions, entry.Name())
		}
	}

	return installedVersions, nil
}
