-- gj/gk in the chat composer: a long line wraps into several screen rows, and gj/gk move between
-- them (keeping the screen column), falling back to whole lines at the edges (so k still leaves
-- the composer for the transcript). vim-display-lines makes j/k do this.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.keys.scheme("vim")

local function st() return d.composer_state() end
local words = {}
for i = 1, 70 do words[i] = string.format("word%02d", i) end
local long = table.concat(words, " ")
local first

local steps = {
  { function() d.text(long) end, nil },
  { "ESC", function()
      first = st().col
      T.check("one long line", st().line == 1 and first == #long, true)
    end },
  { "g k", function()
      local s = st()
      T.check("gk moves up a screen row, in the same line", s.focus == "prompt" and s.line == 1 and s.col < first and s.col > first - 160, true)
    end },
  { "g j", function() T.check("gj comes back to the same column", st().col, first) end },
  { "0 g j", function() T.check("gj from the start lands on the second row's start", st().line == 1 and st().col > 40, true) end },
  { "g k", function() T.check("gk returns to the first row", st().focus == "prompt" and st().col, 1) end },
  { "g k", function() T.check("gk past the first row leaves for the transcript", st().focus, "transcript") end },
  { "i ESC", function() T.check("back in the composer", st().focus, "prompt") end },
  { function() serpis.var.set("vim-display-lines", true); T.press("$") end, nil },
  { "k", function()
      local s = st()
      T.check("with vim-display-lines, k moves a screen row", s.focus == "prompt" and s.line == 1 and s.col < first and s.col > first - 160, true)
    end },
  { "j", function() T.check("and j comes back", st().col, first) end },
  { "g k", function() T.check("gk moves whole lines then (and crosses)", st().focus, "transcript") end },
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
serpis.command.run("new-chat")
serpis.defer(500, go)
