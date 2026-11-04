# serpis shell integration for zsh: OSC 133 prompt marks, so the terminal knows where the
# prompt ends and the command line begins (vim normal state edits only the command line), and
# OSC 7, so it knows the folder the shell is in (what new chats and terminals start in).
(( $+functions[__serpis_precmd] )) && return

__serpis_precmd() {
  builtin local st=$?
  [[ -n $__serpis_prompted ]] && builtin print -n "\e]133;D;$st\a"
  __serpis_prompted=1
  builtin local dir=${PWD//\%/%25}
  dir=${dir//\#/%23}
  builtin print -n "\e]7;file://${dir//\?/%3F}\a"
  # Run after prompt themes that rebuild PROMPT in their own precmd.
  if [[ ${precmd_functions[-1]} != __serpis_precmd ]]; then
    precmd_functions=(${precmd_functions:#__serpis_precmd} __serpis_precmd)
  fi
  if [[ $PROMPT != *$'\e]133;B'* ]]; then
    # A trailing lone % would swallow the `%{` of the mark.
    [[ $PROMPT == *[^%]% || $PROMPT == % ]] && PROMPT=$PROMPT%
    PROMPT=$'%{\e]133;A\a%}'$PROMPT$'%{\e]133;B\a%}'
  fi
}

__serpis_preexec() {
  builtin print -n "\e]133;C\a"
}

builtin autoload -Uz add-zsh-hook
add-zsh-hook precmd __serpis_precmd
add-zsh-hook preexec __serpis_preexec
