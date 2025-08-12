package internal

import (
	"fmt"
	"os"
	"path/filepath"
)

// IsAvmInPath checks if the avm bin directory is in the PATH.
func IsAvmInPath() (bool, error) {
	homeDir, err := os.UserHomeDir()
	if err != nil {
		return false, err
	}
	avmBinDir := filepath.Join(homeDir, ".avm", "bin")

	path := os.Getenv("PATH")
	paths := filepath.SplitList(path)

	for _, p := range paths {
		if p == avmBinDir {
			return true, nil
		}
	}

	return false, nil
}

// ShellConfig describes the configuration for a shell.
type ShellConfig struct {
	Name          string
	ConfigFile    string
	ExportCommand string
}

// GetShellConfig detects the current shell and returns its configuration.
func GetShellConfig() (*ShellConfig, error) {
	shell := os.Getenv("SHELL")
	if shell == "" {
		return nil, fmt.Errorf("could not detect shell: SHELL environment variable not set")
	}

	homeDir, err := os.UserHomeDir()
	if err != nil {
		return nil, err
	}

	shellName := filepath.Base(shell)
	switch shellName {
	case "bash":
		return &ShellConfig{
			Name:          "bash",
			ConfigFile:    filepath.Join(homeDir, ".bashrc"),
			ExportCommand: `export PATH="$HOME/.avm/bin:$PATH"`,
		}, nil
	case "zsh":
		return &ShellConfig{
			Name:          "zsh",
			ConfigFile:    filepath.Join(homeDir, ".zshrc"),
			ExportCommand: `export PATH="$HOME/.avm/bin:$PATH"`,
		}, nil
	case "fish":
		return &ShellConfig{
			Name:          "fish",
			ConfigFile:    filepath.Join(homeDir, ".config", "fish", "config.fish"),
			ExportCommand: `set -gx PATH "$HOME/.avm/bin" $PATH`,
		}, nil
	default:
		return nil, fmt.Errorf("unsupported shell: %s. Please configure your PATH manually", shellName)
	}
}
