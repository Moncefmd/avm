package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/user/avm/internal"
)

var listCmd = &cobra.Command{
	Use:   "list",
	Short: "List all installed versions of argocd",
	Long:  `List all installed versions of argocd.`,
	Run: func(cmd *cobra.Command, args []string) {
		homeDir, err := os.UserHomeDir()
		if err != nil {
			fmt.Println("Error getting home directory:", err)
			os.Exit(1)
		}

		versionsDir := filepath.Join(homeDir, ".avm", "versions")
		if _, err := os.Stat(versionsDir); os.IsNotExist(err) {
			fmt.Println("No versions installed yet.")
			return
		}

		entries, err := os.ReadDir(versionsDir)
		if err != nil {
			fmt.Println("Error reading versions directory:", err)
			os.Exit(1)
		}

		var installedVersions []string
		for _, entry := range entries {
			if entry.IsDir() {
				installedVersions = append(installedVersions, entry.Name())
			}
		}

		if len(installedVersions) == 0 {
			fmt.Println("No versions installed yet.")
			return
		}

		activeVersion, err := internal.GetActiveVersion(homeDir)
		if err != nil {
			fmt.Println("Error getting active version:", err)
			os.Exit(1)
		}

		fmt.Println("Installed versions:")
		for _, version := range installedVersions {
			if version == activeVersion {
				fmt.Printf("* %s (active)\n", version)
			} else {
				fmt.Printf("  %s\n", version)
			}
		}
	},
}

func init() {
	rootCmd.AddCommand(listCmd)
}
