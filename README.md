# avm: ArgoCD Version Manager

`avm` is a command-line tool for managing multiple versions of the `argocd` CLI. It's inspired by tools like `nvm` and `tfenv`, and it makes it easy to install, use, and uninstall different `argocd` versions.

## Installation

### From GitHub Releases (Recommended)

You can download the latest pre-compiled binary for your operating system and architecture from the [GitHub Releases](https://github.com/Moncefmd/avm/releases) page.

Once downloaded, make sure the binary is executable (`chmod +x avm`) and move it to a directory in your `PATH`.

### From Source

If you have Go installed, you can also build `avm` from source:

```sh
git clone https://github.com/Moncefmd/avm.git
cd avm
go build
```

This will create an `avm` binary in the project directory.

## Setup

After installing `avm`, you need to add its `bin` directory to your `PATH`. This is where `avm` will place the symlink to the active `argocd` version.

Add the following line to your shell's configuration file (e.g., `~/.bashrc`, `~/.zshrc`):

```sh
export PATH="$HOME/.avm/bin:$PATH"
```

After adding this line, restart your shell or run `source ~/.bashrc` (or the equivalent for your shell).

## Usage

Here are the commands available in `avm`:

### `avm install <version>`

Install a specific version of `argocd`. You can use a specific version number (e.g., `v2.4.0`) or `latest` to install the most recent stable version.

**Examples:**

```sh
# Install version 2.4.0
avm install v2.4.0

# Install the latest version
avm install latest
```

### `avm use <version>`

Switch the active `argocd` version. The specified version must be already installed.

**Example:**

```sh
avm use v2.4.0
```

### `avm list`

List all installed versions of `argocd`. The currently active version will be marked with an asterisk (`*`).

**Example:**

```sh
$ avm list
Installed versions:
  v2.3.5
* v2.4.0 (active)
  v2.5.0
```

### `avm uninstall <version>`

Uninstall a specific version of `argocd`.

**Example:**

```sh
avm uninstall v2.3.5
```

## Contributing

Contributions are welcome! If you find a bug or have a feature request, please open an issue on the [GitHub repository](https://github.com/Moncefmd/avm/issues).

## License

This project is licensed under the Apache License 2.0. See the [LICENSE](LICENSE) file for details.
