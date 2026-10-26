-- A terminal in its terminal host keeps up: keys typed show at once, a flood of output reaches
-- the screen quickly, and a second terminal opens while the first one prints.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local clock = serpis.time

local function when(ready, go, tries, limit)
  tries, limit = tries or 0, limit or 2000
  if not ready() and tries < limit then return serpis.defer(5, function() when(ready, go, tries + 1, limit) end) end
  go(ready())
end
local times = io.open(os.getenv("SERPIS_TEST_OUT") .. ".times", "w")
local function has(text) return (serpis.buffer.text() or ""):find(text, 1, true) ~= nil end

if os.getenv("SERPIS_TEST_HOST") then serpis.var.set("terminal-persistent", true) end
serpis.command.run("new-terminal")
local first = serpis.buffer.current()
local t0
local steps = {
  function(go) when(function() return has("%") end, function() serpis.defer(500, go) end) end,
  function(go)
    -- Typed one real key at a time, each shown before the next: how long each takes to show.
    local word, i, worst, total = "typedquickly", 0, 0, 0
    local function next_key()
      i = i + 1
      if i > #word then
        times:write(string.format("keys: worst %.1f ms, mean %.1f ms\n", worst * 1000, total / #word * 1000))
        T.check("each key shows within 50 ms", worst < 0.05, true)
        T.press("RET")
        return go()
      end
      local at = clock()
      T.press(word:sub(i, i))
      when(function() return has(word:sub(1, i)) end, function()
        local took = clock() - at
        worst, total = math.max(worst, took), total + took
        next_key()
      end)
    end
    next_key()
  end,
  function(go)
    t0 = clock()
    serpis.buffer.insert("head -c 5000000 /dev/zero | tr '\\0' x; echo; echo flood-$((1+1))\r")
    -- A second terminal opens while the first prints.
    serpis.defer(100, function()
      serpis.command.run("new-terminal")
      when(function() return serpis.buffer.current() ~= first and has("%") end, function(ok)
        T.check("a second terminal opens while the first prints", ok, true)
        go()
      end)
    end)
  end,
  function(go)
    serpis.buffer.show(first)
    when(function() return has("flood-2") end, function(ok)
      T.check("5 MB of output reaches the screen", ok, true)
      times:write(string.format("5 MB: %.2f s\n", clock() - t0))
      T.check("in a few seconds", clock() - t0 < 3.0, true)
      go()
    end)
  end,
}
-- A flood of color: what costs a terminal most (termbench's FGBGPerChar), 40 MB of it.
table.insert(steps, function(go)
  serpis.buffer.show(first)
  local script = os.getenv("SERPIS_TEST_OUT") .. ".colors.py"
  local f = io.open(script, "w")
  f:write("import sys\nrow = ''.join('\\x1b[38;2;%d;%d;%dm\\x1b[48;2;%d;%d;%dmx' % (c%256,(c*3)%256,(c*7)%256,(c*5)%256,(c*11)%256,(c*13)%256) for c in range(2400))\nfor i in range(400):\n    sys.stdout.write(row)\n")
  f:close()
  local gen = "python3 '" .. script .. "'; printf '\\033[0m\\n'; echo colors-$((2+2))\r"
  t0 = clock()
  serpis.buffer.insert(gen)
  when(function() return has("colors-4") end, function(ok)
    times:write(string.format("40 MB of colors: %.2f s\n", clock() - t0))
    T.check("a flood of colors gets through", ok, true)
    T.check("in a few seconds", clock() - t0 < 8.0, true)
    go()
  end, 0, 4000)
end)
-- The real termbench, when there is one to run (SERPIS_TEST_TERMBENCH: its path).
if os.getenv("SERPIS_TEST_TERMBENCH") then
  table.insert(steps, function(go)
    t0 = clock()
    serpis.buffer.insert(os.getenv("SERPIS_TEST_TERMBENCH") .. " small; echo bench-$((5+5))\r")
    when(function() return has("bench-10") end, function(ok)
      times:write(string.format("termbench small: %.2f s\n", clock() - t0))
      go()
    end, 0, 40000)
  end)
end

local i = 0
local function go()
  i = i + 1
  if not steps[i] then times:close(); return T.finish() end
  steps[i](go)
end
go()
