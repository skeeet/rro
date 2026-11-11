# Installation

## Binary archive

Extract the Apple Silicon Mac archive and enter its directory:

```sh
tar -xzf rro-0.1.1-darwin-arm64.tar.gz
cd rro-0.1.1-darwin-arm64
sh ./install.sh
export PATH="$HOME/.local/bin:$PATH"
rro --version
```

The installer copies `rro` to `$HOME/.local/bin/rro`. It leaves shell startup files
and macOS security settings unchanged. You can also run `./rro` directly from the
extracted directory.

To make the command available in new terminals, add this line to your shell startup
file (`~/.zprofile` for the default macOS login shell):

```sh
export PATH="$HOME/.local/bin:$PATH"
```

## Updates and custom locations

Existing installations are preserved unless you pass `--force`. From a newly
extracted archive, update with:

```sh
sh ./install.sh --force
```

Use `--prefix DIR` to install to `DIR/bin/rro`, then add that directory to `PATH`:

```sh
sh ./install.sh --prefix "$HOME/tools"
export PATH="$HOME/tools/bin:$PATH"
```

Run `sh ./install.sh --help` for options. To remove the default installation:

```sh
rm "$HOME/.local/bin/rro"
```

For a custom prefix, remove `DIR/bin/rro` instead.

## Source checkout

[Build the executable](REBUILD.md#build-the-executable), then install that build:

```sh
sh scripts/install.sh target/release/rro
export PATH="$HOME/.local/bin:$PATH"
rro --version
```

Without an explicit binary path, `sh scripts/install.sh` selects `dist/rro`, falling
back to `target/release/rro`. It accepts the same `--force` and `--prefix` options.
