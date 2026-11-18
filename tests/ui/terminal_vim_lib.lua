-- Shared by terminal_vim*.lua: serpis's vim normal state editing a shell's command line,
-- through real key input. Each case types a line, presses keys, and checks what the shell has.
local T

local function when(ready, f, n)
  n = n or 150
  if ready() or n == 0 then return f() end
  serpis.defer(100, function() when(ready, f, n - 1) end)
end
local function state() return serpis.buffer.modes()[1] end

-- The command line after the prompt symbol `sym` and a space (the last prompt line; an empty
-- one is just the symbol: lines are trimmed).
local function cmdline(sym)
  local last
  for l in (serpis.buffer.text() .. "\n"):gmatch("(.-)\n") do
    if l:sub(1, #sym) == sym then last = l end
  end
  return last and (last:sub(#sym + 2):gsub("%s+$", "")) or "<no prompt>"
end

-- { name, text, keys, want [, then_insert] }: type `text`, press `keys`, then optionally
-- insert `then_insert` (keys must leave insert state), and compare the line with `want`.
local cases = {
  { "0 dw deletes the first word", "echo aa bb", "ESC 0 d w", "aa bb" },
  { "^ dw deletes the first word", "echo aa bb", "ESC ^ d w", "aa bb" },
  { "0 x deletes the first char", "echo aa bb", "ESC 0 x", "cho aa bb" },
  { "0 de deletes to the first word's end", "echo aa bb", "ESC 0 d e", " aa bb" },
  { "0 2dw deletes two words", "echo aa bb", "ESC 0 2 d w", "bb" },
  { "0 d$ empties the line", "echo aa bb", "ESC 0 d $", "" },
  { "0 cw changes the first word", "echo aa bb", "ESC 0 c w", "printf aa bb", "printf" },
  { "dw in the middle", "echo aa bb", "ESC 0 w d w", "echo bb" },
  { "dw on the last word", "echo aa bb", "ESC $ b d w", "echo aa" },
  { "db from the end", "echo aa bb", "ESC $ d b", "echo aa b" },
  { "d0 from the end", "echo aa bb", "ESC $ d 0", "b" },
  { "D from the second word", "echo aa bb", "ESC 0 w D", "echo" },
  { "dW over a path", "echo /a/b c", "ESC 0 w d W", "echo c" },
  { "$ x deletes the last char", "echo aa bb", "ESC $ x", "echo aa b" },
  { "dw then u undoes", "echo aa bb", "ESC 0 w d w u", "echo aa bb" },
  { "2dw then u undoes both words", "echo aa bb", "ESC 0 2 d w u", "echo aa bb" },
  { "x x u undoes one x", "echo aa bb", "ESC $ x x u", "echo aa b" },
  { "x x u u undoes both", "echo aa bb", "ESC $ x x u u", "echo aa bb" },
  { "dw u C-r redoes", "echo aa bb", "ESC 0 w d w u C-r", "echo bb" },
  { "u then x keeps editing", "echo aa bb", "ESC 0 d w u x", "cho aa bb" },
  { "dw then $ p pastes", "echo aa bb", "ESC 0 w d w $ p", "echo bbaa" },
  { "cc replaces the line", "echo aa bb", "ESC c c", "ls", "ls" },
  { "ESC i inserts before the last char", "echo aa bb", "ESC i", "echo aa bxb", "x" },
  { "ESC a appends", "echo aa bb", "ESC a", "echo aa bbx", "x" },
  { "0 i inserts at the start", "echo aa bb", "ESC 0 i", "xecho aa bb", "x" },
  { "I inserts at the start", "echo aa bb", "ESC I", "xecho aa bb", "x" },
  { "C-w deletes the last word", "echo aa bb", "C-w", "echo aa" },
  { "C-w twice", "echo aa bb", "C-w C-w", "echo" },
  { "C-w after moving left in normal", "echo aa bb", "ESC 0 w w i C-w", "echo bb" },
  { "C-w after A", "echo aa bb", "ESC 0 A C-w", "echo aa" },
  { "C-u deletes to the start", "echo aa bb", "C-u", "" },
  -- C-u goes to the shell: zsh's emacs keymap kills the whole line.
  { "C-u after moving left goes to the shell", "echo aa bb", "ESC 0 w w i C-u", "" },
  { "backspace after moving left", "echo aa bb", "ESC 0 w w i DEL", "echo aabb" },
  { "h stops at the prompt", "echo aa bb", "ESC 0 h x", "cho aa bb" },
  { "b stops at the prompt", "echo aa bb", "ESC 0 w b x", "cho aa bb" },
}

local function run(cases, km, sym, i, done)
  if i > #cases then return done() end
  local c = cases[i]
  -- A fresh line: C-c C-c sends ^C, so the shell drops the line and prompts again.
  if state() ~= "vim-insert" then T.press("i") end
  T.press("C-c C-c")
  serpis.defer(300, function()
    serpis.buffer.insert(c[2])
    serpis.defer(300, function()
      T.press(c[3])
      serpis.defer(400, function()
        local function check()
          T.check("[" .. km .. "] " .. c[1], cmdline(sym), c[4])
          run(cases, km, sym, i + 1, done)
        end
        if c[5] then
          serpis.buffer.insert(c[5])
          serpis.defer(300, check)
        else
          check()
        end
      end)
    end)
  end)
end

local function each_config(T, configs, cases, k)
  if k > #configs then
    T.press("C-c C-c")
    return T.finish()
  end
  local label, setup, sym, before = configs[k][1], configs[k][2], configs[k][3], configs[k][4]
  local function go()
    if state() ~= "vim-insert" then T.press("i") end
    T.press("C-c C-c")
    serpis.defer(300, function()
      serpis.buffer.insert(setup .. "\r")
      serpis.defer(500, function()
        run(cases, label, sym, 1, function() each_config(T, configs, cases, k + 1) end)
      end)
    end)
  end
  if k == 1 or before then
    if before then before() end
    serpis.command.run("new-terminal")
    -- Wait for the prompt, and a little longer for the line editor to start reading keys.
    when(function() return serpis.buffer.text():find("%S") end, function() serpis.defer(700, go) end)
  else
    go()
  end
end

-- Run `cases` (all, or those whose names `only` lists) in each of `configs`:
-- { label, shell setup, prompt symbol [, serpis setup before its own new terminal] }.
return function(T_, configs, only)
  T = T_
  serpis.keys.scheme("vim")
  serpis.var.set("confirm-quit-processes", "never")
  local picked = cases
  if only then
    picked = {}
    for _, c in ipairs(cases) do
      if only[c[1]] then table.insert(picked, c) end
    end
  end
  each_config(T, configs, picked, 1)
end
