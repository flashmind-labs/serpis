# serpis shell integration for zsh, loaded because serpis pointed ZDOTDIR here.
# Put ZDOTDIR back, run the user's own .zshenv, then add the prompt marks.
if [[ -n "${SERPIS_ZDOTDIR+x}" ]]; then
  builtin export ZDOTDIR="$SERPIS_ZDOTDIR"
  builtin unset SERPIS_ZDOTDIR
else
  builtin unset ZDOTDIR
fi
[[ -r "${ZDOTDIR:-$HOME}/.zshenv" ]] && builtin source "${ZDOTDIR:-$HOME}/.zshenv"
[[ -o interactive && -r "$SERPIS_SHELL_DIR/serpis.zsh" ]] && builtin source "$SERPIS_SHELL_DIR/serpis.zsh"
