# Sourced by dsterm-managed bash sessions via `bash --rcfile <this> -i`.
# Sources the user's normal rcfile first, then installs DS shell integration.
if [ -f "$HOME/.bashrc" ]; then
  . "$HOME/.bashrc"
fi

__dsterm_postexec() {
  local ec=$?
  printf '\033]633;D;%s\033\\' "$ec"
}

if [ -z "$PROMPT_COMMAND" ]; then
  PROMPT_COMMAND='__dsterm_postexec'
else
  case "$PROMPT_COMMAND" in
    *__dsterm_postexec*) ;;
    *) PROMPT_COMMAND='__dsterm_postexec;'"$PROMPT_COMMAND" ;;
  esac
fi
