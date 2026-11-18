-- Vim normal state on a plain shell's command line (zsh, emacs keymap): motions, operators,
-- undo and the insert-state keys that go to the shell (cases in terminal_vim_lib.lua).
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
dofile(os.getenv("SERPIS_TEST_DIR") .. "/terminal_vim_lib.lua")(T, { { "zsh", "bindkey -e", "%" } })
