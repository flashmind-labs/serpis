-- The same vim editing behind a non-ASCII prompt (`☧ `): marked by serpis's shell integration
-- (here with zsh's vi keymap, which zsh picks when $EDITOR mentions vi), and without the
-- integration, found by a `terminal-prompt-regexp` that knows the symbol.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local only = {}
for _, n in ipairs({ "0 dw deletes the first word", "^ dw deletes the first word", "0 x deletes the first char",
  "dw in the middle", "D from the second word", "dw then u undoes", "dw u C-r redoes", "cc replaces the line",
  "I inserts at the start", "C-w after moving left in normal", "h stops at the prompt", "b stops at the prompt" }) do
  only[n] = true
end
dofile(os.getenv("SERPIS_TEST_DIR") .. "/terminal_vim_lib.lua")(T, {
  { "☧", "bindkey -v; PROMPT='☧ '", "☧" },
  { "☧, no integration", "bindkey -e; PROMPT='☧ '", "☧", function()
    serpis.var.set("terminal-shell-integration", false)
    serpis.var.set("terminal-prompt-regexp", "^[^#$%>λ❯→➜☧]{0,60}[#$%>λ❯→➜☧]+[ \\x{a0}]*")
  end },
}, only)
