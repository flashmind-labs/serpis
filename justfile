# Serpis' common tasks. `just` alone lists them; `just <task> [flags]` runs one.
#
# libghostty-vt is built from source with zig 0.16, so every recipe runs with zig on PATH: brew's
# zig@0.16 keg (not necessarily linked), or the directory in ZIG_BIN. `brew install zig@0.16` is
# all it takes on macOS.

zig_bin := env_var_or_default("ZIG_BIN", trim(shell("""
    d=$(brew --prefix zig@0.16 2>/dev/null)
    if [ -z "$d" ]; then
        for c in /opt/homebrew/opt/zig@0.16 /usr/local/opt/zig@0.16; do
            if [ -x "$c/bin/zig" ]; then d=$c; break; fi
        done
    fi
    printf %s "$d"
""", "")))
export PATH := if zig_bin == "" {
    env_var_or_default("PATH", "")
} else {
    zig_bin + "/bin:" + env_var_or_default("PATH", "")
}

# List the tasks
default:
    @just --list

# Build the workspace
build:
    cargo build

# Build Serpis.app into ~/Applications and `serpis` into ~/.local/bin
install:
    scripts/install-macos.sh

# Build the iPhone app and install it on a paired phone (`just install-ios --build-only`)
install-ios *args:
    scripts/install-ios.sh {{args}}

# Run serpis
run:
    cargo run -p serpis-bin

# Unit tests
test:
    cargo test

# Scripted UI tests against a real window
ui-test:
    scripts/ui-test.sh

# The same UI tests at 760x560, where crowded layouts show
narrow:
    scripts/ui-test.sh --narrow

# Build serpis-node for a box: release, one file, std linked in (`just node x86_64-unknown-linux-gnu.2.28`)
node *args:
    scripts/build-node.sh {{args}}

# Run a whole chat over real ssh: `just ssh-check host`
ssh-check host="localhost":
    scripts/ssh-check.sh {{host}}

# Prompt caching end to end against OpenRouter (spends a few cents; needs OPENROUTER_API_KEY)
live-cache-check:
    scripts/live-cache-check.sh

# The promo video, from a scripted run (`just promo --compose` reuses the last take)
promo *args:
    scripts/promo/build.sh {{args}}

# Draw the logo and app icon into assets/
icon:
    python3 scripts/make-icon.py

# Format
fmt:
    cargo fmt

# Lint
clippy:
    cargo clippy --all-targets

# Remove build output
clean:
    cargo clean
