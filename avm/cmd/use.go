package cmd

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
	"github.com/Moncefmd/avm/internal"
)

var useCmd = &cobra.Command{
	Use:   "use [version]",
	Short: "Switch to a specific version of argocd",
	Long:  `Switch to a specific version of argocd.`,
	Args:  cobra.ExactArgs(1),
	Run: func(cmd *cobra.Command, args []string) {
		version := args[0]
		if err := internal.UseVersion(version); err != nil {
			fmt.Println(err)
			os.Exit(1)
		}
		fmt.Printf("Now using argocd version %s\n", version)
	},
}

func init() {
	rootCmd.AddCommand(useCmd)
}
