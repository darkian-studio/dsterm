# Loaded when ZDOTDIR points to dsterm's integration dir.
# Source the user's real .zshrc first if present.
if [ -f "$HOME/.zshrc" ]; then
  . "$HOME/.zshrc"
fi

__dsterm_postexec() {
  printf '\033]633;D;%s\033\\' "$?"
}
precmd_functions+=(__dsterm_postexec)
