function __dsterm_postexec --on-event fish_postexec
  printf '\033]633;D;%s\033\\' $status
end
