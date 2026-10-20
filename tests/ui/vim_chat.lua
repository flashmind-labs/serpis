-- Vim in the chat composer, through real keys: motions stay inside the composer, visual and
-- visual-line selections behave like vim, and the transcript is a separate, explicit focus.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.keys.scheme("vim")

local function st() return d.composer_state() end
local function at(l, c) local s = st(); return s.line == l and s.col == c end
local function text() return st().text end
local function mode() return serpis.buffer.modes()[1] end

-- Each step: keys to press, then a check of the state they lead to.
local steps = {
  { function() d.text("alpha beta"); T.press("S-RET"); d.text("gamma delta"); T.press("S-RET"); d.text("epsilon zeta") end, nil },
  { "ESC", function() T.check("ESC: normal, back one column", mode() == "vim-normal" and at(3, 12), true) end },
  { "k k", function() T.check("k moves up the composer", st().focus == "prompt" and at(1, 10), true) end },
  { "k", function() T.check("k on its first line moves up into the transcript", st().focus, "transcript") end },
  { "j", function() T.check("j on the transcript's last line comes back down", st().focus == "prompt" and st().line == 1, true) end },
  { "j", function() T.check("j keeps the column", at(2, 11), true) end },
  { "g g", function() T.check("gg: composer's first line", st().focus == "prompt" and at(1, 1), true) end },
  { "G", function() T.check("G: composer's last line", at(3, 1), true) end },
  -- $ goes to 3:12, b to "zeta" (3:9); k then keeps column 9 instead of the old $ goal.
  { "$ b k", function() T.check("a horizontal motion resets the goal column", at(2, 9), true) end },
  { "v k h", function() T.check("visual extends upward", mode() == "vim-visual" and at(1, 8), true) end },
  -- From "e" in "beta" through "l" in "delta", both ends included.
  { "d", function() T.check("visual d deletes the selection inclusively", text(), "alpha bta\nepsilon zeta") end },
  { "u", function() T.check("u restores", text(), "alpha beta\ngamma delta\nepsilon zeta") end },
  { "j V", function() T.check("V keeps the cursor column", mode() == "vim-visual-line" and at(2, 8), true) end },
  { "k", function() T.check("V k moves the cursor, still in the composer", st().focus == "prompt" and at(1, 8), true) end },
  { "k", function() T.check("V k at the top stays", at(1, 8) and mode() == "vim-visual-line", true) end },
  { "y", function() T.check("V y yanks and leaves visual", mode(), "vim-normal") end },
  { "G p", function() T.check("p pastes the yanked lines below", text(), "alpha beta\ngamma delta\nepsilon zeta\nalpha beta\ngamma delta") end },
  { "g g d d", function() T.check("dd", text(), "gamma delta\nepsilon zeta\nalpha beta\ngamma delta") end },
  { "$ x", function() T.check("x at the line end", text(), "gamma delt\nepsilon zeta\nalpha beta\ngamma delta") end },
  { "j c i w", function() T.check("ciw enters insert", mode(), "vim-insert") end },
  { function() d.text("omega"); T.press("ESC") end, function() T.check("ciw replaced the word", text(), "gamma delt\nepsilon omega\nalpha beta\ngamma delta") end },
  { "C-k", function() T.check("C-k focuses the transcript", st().focus, "transcript") end },
  { "i", function() T.check("i from the transcript edits the composer", st().focus == "prompt" and mode() == "vim-insert", true) end },
  { "ESC", nil },
  -- A label jump from the composer into the transcript ("No model configured…" is in it).
  { "g w", function()
      local l
      for _, x in ipairs(serpis.jump.labels()) do if x.part == 0 then l = x; break end end
      T.check("gw labels transcript words", l ~= nil, true)
      if l then T.press((l.label:gsub(".", "%0 "))) end
    end },
  { function() end, function() T.check("gw from the composer lands in the transcript", st().focus, "transcript") end },
  { "g g G", function() T.check("motions there leave the composer alone", text(), "gamma delt\nepsilon omega\nalpha beta\ngamma delta") end },
  { "i", function() T.check("i returns to the composer", st().focus == "prompt" and mode() == "vim-insert", true) end },
}
local i = 0
local function go()
  i = i + 1
  local s = steps[i]
  if not s then return T.finish() end
  if type(s[1]) == "string" then T.press(s[1]) else s[1]() end
  serpis.defer(250, function()
    if s[2] then s[2]() end
    go()
  end)
end
serpis.defer(500, go)
