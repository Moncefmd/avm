package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
)

var useCmd = &cobra.Command{
	Use:   "use [version]",
	Short: "Switch to a specific version of argocd",
	Long:  `Switch to a specific version of argocd.`,
	Args:  cobra.ExactArgs(1),
	Run: func(cmd *cobra.Command, args []string) {
		version := args[0]

		homeDir, err := os.UserHomeDir()
		if err != nil {
			fmt.Println("Error getting home directory:", err)
			os.Exit(1)
		}

		versionDir := filepath.Join(homeDir, ".avm", "versions", version)
		binaryPath := filepath.Join(versionDir, "argocd")

		if _, err := os.Stat(binaryPath); os.IsNotExist(err) {
			fmt.Printf("Version %s is not installed. Please run 'avm install %s' first.\n", version, version)
			os.Exit(1)
		}

		binDir := filepath.Join(homeDir, ".avm", "bin")
		if err := os.MkdirAll(binDir, 0755); err != nil {
			fmt.Println("Error creating bin directory:", err)
			os.Exit(1)
		}

		symlinkPath := filepath.Join(binDir, "argocd")

		// Remove existing symlink
		if _, err := os.Lstat(symlinkPath); err == nil {
			if err := os.Remove(symlinkPath); err != nil {
				fmt.Println("Error removing existing symlink:", err)
				os.Exit(1)
			}
		}

		if err := os.Symlink(binaryPath, symlinkPath); err != nil {
			fmt.Println("Error creating symlink:", err)
			os.Exit(1)
		}

		fmt.Printf("Now using argocd version %s\n", version)
	},
}

func init() {
	rootCmd.AddCommand(useCmd)
}
