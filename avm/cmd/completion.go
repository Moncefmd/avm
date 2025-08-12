package cmd

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"
)

var completionCmd = &cobra.Command{
	Use:   "completion [bash|zsh|fish|powershell]",
	Short: "Generate completion script",
	Long: `To load completions:

Bash:
  $ source <(avm completion bash)

  # To load completions for each session, execute once:
  # Linux:
  $ avm completion bash > /etc/bash_completion.d/avm
  # macOS:
  $ avm completion bash > /usr/local/etc/bash_completion.d/avm

Zsh:
  # If shell completion is not already enabled in your environment,
  # you will need to enable it.  You can execute the following once:

  $ echo "autoload -U compinit; compinit" >> ~/.zshrc

  # To load completions for each session, execute once:
  $ avm completion zsh > "${fpath[1]}/_avm"

  # You will need to start a new shell for this setup to take effect.

Fish:
  $ avm completion fish | source

  # To load completions for each session, execute once:
  $ avm completion fish > ~/.config/fish/completions/avm.fish

Powershell:
  PS> avm completion powershell | Out-String | Invoke-Expression

  # To load completions for every new session, run:
  PS> avm completion powershell > avm.ps1
  # and source this file from your PowerShell profile.
`,
	DisableFlagsInUseLine: true,
	ValidArgs:             []string{"bash", "zsh", "fish", "powershell"},
	Args:                  cobra.ExactValidArgs(1),
	Run: func(cmd *cobra.Command, args []string) {
		install, _ := cmd.Flags().GetBool("install")
		shell := args[0]
		if install {
			if err := installCompletion(shell); err != nil {
				fmt.Println(err)
				os.Exit(1)
			}
		} else {
			if err := generateCompletionScript(os.Stdout, shell); err != nil {
				fmt.Println(err)
				os.Exit(1)
			}
		}
	},
}

func init() {
	completionCmd.Flags().Bool("install", false, "Install the completion script automatically")
	rootCmd.AddCommand(completionCmd)
}

func installCompletion(shell string) error {
	homeDir, err := os.UserHomeDir()
	if err != nil {
		return err
	}

	var completionFile string
	switch shell {
	case "bash":
		fmt.Println("Bash completion installation is not fully automated.")
		fmt.Println("Please add the following line to your ~/.bashrc or ~/.bash_profile:")
		fmt.Println("  source <(avm completion bash)")
		return nil
	case "zsh":
		completionFile = filepath.Join(homeDir, ".zsh", "completions", "_avm")
	case "fish":
		completionFile = filepath.Join(homeDir, ".config", "fish", "completions", "avm.fish")
	case "powershell":
		fmt.Println("PowerShell completion installation is not automated.")
		fmt.Println("Please run `avm completion powershell > avm.ps1` and source it from your profile.")
		return nil
	default:
		return fmt.Errorf("unsupported shell: %s", shell)
	}

	fmt.Printf("This will install the completion script to %s. Continue? [y/n] ", completionFile)

	reader := bufio.NewReader(os.Stdin)
	input, _ := reader.ReadString('\n')
	if strings.TrimSpace(strings.ToLower(input)) != "y" {
		fmt.Println("Installation cancelled.")
		return nil
	}

	dir := filepath.Dir(completionFile)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return fmt.Errorf("failed to create directory %s: %w", dir, err)
	}

	file, err := os.Create(completionFile)
	if err != nil {
		return fmt.Errorf("failed to create completion file: %w", err)
	}
	defer file.Close()

	if err := generateCompletionScript(file, shell); err != nil {
		return fmt.Errorf("failed to generate completion script: %w", err)
	}

	fmt.Println("Completion script installed successfully.")
	return nil
}

func generateCompletionScript(file *os.File, shell string) error {
	switch shell {
	case "bash":
		return rootCmd.GenBashCompletion(file)
	case "zsh":
		return rootCmd.GenZshCompletion(file)
	case "fish":
		return rootCmd.GenFishCompletion(file, true)
	case "powershell":
		return rootCmd.GenPowerShellCompletion(file)
	default:
		return fmt.Errorf("unsupported shell: %s", shell)
	}
}
