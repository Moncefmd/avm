package cmd

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
)

var rootCmd = &cobra.Command{
	Use:   "avm",
	Short: "avm is a version manager for the argocd CLI",
	Long: `avm (ArgoCD Version Manager) is a command-line tool to manage multiple versions of the argocd CLI.
You can install, use, and uninstall different versions of argocd with ease.`,
}

func Execute() {
	if err := rootCmd.Execute(); err != nil {
		fmt.Println(err)
		os.Exit(1)
	}
}
