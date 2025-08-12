package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
	"github.com/Moncefmd/avm/internal"
)

var installCmd = &cobra.Command{
	Use:   "install [version]",
	Short: "Install a specific version of argocd",
	Long:  `Install a specific version of argocd. Use "latest" to install the latest version.`,
	Args:  cobra.ExactArgs(1),
	Run: func(cmd *cobra.Command, args []string) {
		version := args[0]

		if version == "latest" {
			fmt.Println("Finding latest version...")
			latestVersion, err := internal.GetLatestVersion()
			if err != nil {
				fmt.Println("Error getting latest version:", err)
				os.Exit(1)
			}
			version = latestVersion
			fmt.Printf("Latest version is %s\n", version)
		}

		fmt.Printf("Installing argocd version %s...\n", version)

		release, err := internal.GetRelease(version)
		if err != nil {
			fmt.Println("Error getting release information:", err)
			os.Exit(1)
		}

		assetName := internal.GetAssetName()
		var downloadURL string
		for _, asset := range release.Assets {
			if asset.Name == assetName {
				downloadURL = asset.BrowserDownloadURL
				break
			}
		}

		if downloadURL == "" {
			osName, archName := internal.GetPlatform()
			fmt.Printf("Could not find a binary for your platform (%s/%s) for version %s\n", osName, archName, version)
			os.Exit(1)
		}

		fmt.Printf("Downloading from %s\n", downloadURL)

		homeDir, err := os.UserHomeDir()
		if err != nil {
			fmt.Println("Error getting home directory:", err)
			os.Exit(1)
		}

		versionDir := filepath.Join(homeDir, ".avm", "versions", version)
		if err := os.MkdirAll(versionDir, 0755); err != nil {
			fmt.Println("Error creating version directory:", err)
			os.Exit(1)
		}

		binaryPath := filepath.Join(versionDir, "argocd")

		if err := internal.DownloadFile(downloadURL, binaryPath); err != nil {
			fmt.Println("Error downloading argocd binary:", err)
			os.Exit(1)
		}

		if err := os.Chmod(binaryPath, 0755); err != nil {
			fmt.Println("Error making argocd binary executable:", err)
			os.Exit(1)
		}

		fmt.Printf("argocd version %s installed successfully!\n", version)

		// Automatically use the installed version
		if err := internal.UseVersion(version); err != nil {
			fmt.Println("Error switching to new version:", err)
			os.Exit(1)
		}
		fmt.Printf("Now using argocd version %s\n", version)
	},
}

func init() {
	rootCmd.AddCommand(installCmd)
}
