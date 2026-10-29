-- Clicking several times in a row, with the mouse as a person would: two clicks select a word,
-- three its line, four everything; in the regular keys and in vim's.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug

local text = "first line here\nsecond skip_unknown word, and more\nthird line"
local id = serpis.buffer.create("text", "*clicks*", { text = text })
serpis.buffer.show(id)

-- The middle of character `col` of line `line` (1-based) on screen.
local function point(line, col)
  local r = d.cell_rect(line, col)
  return r.x + 2, r.y + r.h / 2
end
local function click(x, y)
  d.mouse_move(x, y)
  d.mouse_button(x, y, "left", true)
  d.mouse_button(x, y, "left", false)
end
-- `n` clicks in a row, each in its own frame (as a hand delivers them).
local function clicks(n, line, col, go)
  local x, y = point(line, col)
  local i = 0
  local function next()
    i = i + 1
    click(x, y)
    if i < n then return serpis.defer(60, next) end
    serpis.defer(250, go)
  end
  next()
end

local steps = {
  function(go) serpis.defer(400, go) end,
  function(go) clicks(2, 2, 10, go) end, -- in skip_unknown
  function(go)
    T.check("double-click selects a word (snake_case is one)", serpis.buffer.region(), "skip_unknown")
    serpis.defer(700, function() clicks(2, 1, 3, go) end)
  end,
  function(go)
    T.check("double-click another word", serpis.buffer.region(), "first")
    serpis.defer(700, function() clicks(3, 2, 3, go) end)
  end,
  function(go)
    T.check("triple-click selects the line", serpis.buffer.region(), "second skip_unknown word, and more")
    serpis.defer(700, function() clicks(4, 3, 2, go) end)
  end,
  function(go)
    T.check("four clicks select everything", serpis.buffer.region(), text)
    serpis.defer(700, function() clicks(1, 1, 2, go) end)
  end,
  function(go)
    T.check("one click clears it", serpis.buffer.region(), nil)
    -- In vim's keys too.
    serpis.keys.scheme("vim")
    serpis.defer(700, function() clicks(2, 3, 2, go) end)
  end,
  function(go)
    T.check("vim: double-click selects a word", serpis.buffer.region(), "third")
    go()
  end,
}
local i = 0
local function go()
  i = i + 1
  if steps[i] then steps[i](go) else T.finish() end
end
go()
