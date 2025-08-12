package cmd

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
	"github.com/Moncefmd/avm/internal"
)

var listCmd = &cobra.Command{
	Use:   "list",
	Short: "List all installed versions of argocd",
	Long:  `List all installed versions of argocd.`,
	Run: func(cmd *cobra.Command, args []string) {
		installedVersions, err := internal.GetInstalledVersions()
		if err != nil {
			fmt.Println("Error getting installed versions:", err)
			os.Exit(1)
		}

		if len(installedVersions) == 0 {
			fmt.Println("No versions installed yet.")
			return
		}

		homeDir, err := os.UserHomeDir()
		if err != nil {
			fmt.Println("Error getting home directory:", err)
			os.Exit(1)
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
