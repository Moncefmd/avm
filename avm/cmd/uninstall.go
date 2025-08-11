package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/user/avm/internal"
)

var uninstallCmd = &cobra.Command{
	Use:   "uninstall [version]",
	Short: "Uninstall a specific version of argocd",
	Long:  `Uninstall a specific version of argocd.`,
	Args:  cobra.ExactArgs(1),
	Run: func(cmd *cobra.Command, args []string) {
		version := args[0]

		homeDir, err := os.UserHomeDir()
		if err != nil {
			fmt.Println("Error getting home directory:", err)
			os.Exit(1)
		}

		versionDir := filepath.Join(homeDir, ".avm", "versions", version)

		if _, err := os.Stat(versionDir); os.IsNotExist(err) {
			fmt.Printf("Version %s is not installed.\n", version)
			os.Exit(1)
		}

		// Check if the version to be uninstalled is the active one.
		activeVersion, err := internal.GetActiveVersion(homeDir)
		if err != nil {
			fmt.Println("Error getting active version:", err)
			os.Exit(1)
		}

		if activeVersion == version {
			symlinkPath := filepath.Join(homeDir, ".avm", "bin", "argocd")
			if err := os.Remove(symlinkPath); err != nil {
				// Ignore error if symlink doesn't exist for some reason
				if !os.IsNotExist(err) {
					fmt.Println("Error removing active version symlink:", err)
					os.Exit(1)
				}
			}
			fmt.Printf("Uninstalled active version %s. Please select a new version with 'avm use <version>'.\n", version)
		}

		if err := os.RemoveAll(versionDir); err != nil {
			fmt.Println("Error uninstalling version:", err)
			os.Exit(1)
		}

		if activeVersion != version {
			fmt.Printf("Version %s has been uninstalled.\n", version)
		}
	},
}

func init() {
	rootCmd.AddCommand(uninstallCmd)
}
