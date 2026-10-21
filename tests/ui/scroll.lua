-- Keyboard scrolling eases instead of jumping, and follows the cursor's screen row (not its
-- whole line, which in a chat can be a paragraph taller than the window).
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.keys.scheme("vim")

local lines = {}
for i = 1, 200 do lines[i] = "line " .. i end
local words = {}
for i = 1, 1500 do words[i] = "w" .. i end
-- Line 201: one line that wraps into far more rows than the window holds.
table.insert(lines, table.concat(words, " "))
table.insert(lines, "after")
local buf = serpis.buffer.create("text", "*scroll*", { text = table.concat(lines, "\n") })
serpis.buffer.show(buf)

local function top() return (serpis.buffer.visible()) end
local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    local s = list[i]
    if not s then return T.finish() end
    local wait = s(go)
    if wait ~= false then serpis.defer(wait or 300, go) end
  end
  go()
end

local start, samples = nil, {}
steps({
  function() T.press("g g") end,
  function(go)
    start = top()
    -- 150 lines down: the view has to scroll; it eases there over a few frames.
    serpis.command.run("vim-normal-state")
    for _ = 1, 150 do T.press("j") end
    local n = 0
    local function sample()
      n = n + 1
      table.insert(samples, top())
      if n < 12 then serpis.defer(16, sample) else serpis.defer(600, go) end
    end
    serpis.defer(16, sample)
    return false
  end,
  function()
    local final = top()
    T.check("the view moved", final > start, true)
    local mid = false
    for _, s in ipairs(samples) do if s > start and s < final then mid = true end end
    T.check("passing through positions in between (eased)", mid, true)
    local l = serpis.buffer.cursor()
    local first, last = serpis.buffer.visible()
    T.check("and settles with the cursor in view", l >= first and l <= last, true)
    -- A line taller than its view: a long prompt in a chat's composer (it scrolls past 40% of
    -- the window). Walking down its rows with gj keeps the cursor's row inside the composer.
    serpis.var.set("scroll-animation-speed", 0)
    serpis.command.run("new-chat")
  end,
  function()
    d.text(table.concat(words, " "))
  end,
  function()
    T.press("ESC 0")
  end,
  function(go)
    local card = d.composer().card
    local n, ok = 0, true
    local function step()
      n = n + 1
      T.press("g j")
      serpis.defer(40, function()
        local s = d.composer_state()
        local r = d.cell_rect(s.line, s.col)
        if s.focus ~= "prompt" or s.line ~= 1 or not r or r.y < card.y or r.y + r.h > card.y + card.h then
          ok = false
          table.insert(T.results, string.format("INFO gj %d: line=%s col=%s rect=%s card=%s..%s", n, s.line, s.col, r and (r.y .. "+" .. r.h) or "nil", card.y, card.y + card.h))
        end
        if n < 40 and ok then step() else
          T.check("gj through a line taller than its view keeps the cursor's row on screen", ok and n == 40, true)
          go()
        end
      end)
    end
    step()
    return false
  end,
})
