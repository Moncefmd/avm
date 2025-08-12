package cmd

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
	"github.com/Moncefmd/avm/internal"
)

var listRemoteCmd = &cobra.Command{
	Use:   "list-remote [version]",
	Short: "List remote versions available for download",
	Long:  `List remote versions available for download. If a version is specified, it lists the available binaries for that version.`,
	Args:  cobra.MaximumNArgs(1),
	Run: func(cmd *cobra.Command, args []string) {
		if len(args) == 0 {
			// List all available versions
			fmt.Println("Available versions:")
			versions, err := internal.GetRemoteVersions()
			if err != nil {
				fmt.Println("Error getting releases:", err)
				os.Exit(1)
			}
			for _, version := range versions {
				fmt.Println(version)
			}
		} else {
			// List available binaries for a specific version
			version := args[0]
			fmt.Printf("Available binaries for version %s:\n", version)
			release, err := internal.GetRelease(version)
			if err != nil {
				fmt.Println("Error getting release information:", err)
				os.Exit(1)
			}
			for _, asset := range release.Assets {
				fmt.Println(asset.Name)
			}
		}
	},
}

func init() {
	rootCmd.AddCommand(listRemoteCmd)
}
