-- A terminal in its terminal host keeps up: keys typed show at once, a flood of output reaches
-- the screen quickly, and a second terminal opens while the first one prints.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local clock = serpis.time

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 2000 then return serpis.defer(5, function() when(ready, go, tries + 1) end) end
  go(ready())
end
local times = io.open(os.getenv("SERPIS_TEST_OUT") .. ".times", "w")
local function has(text) return (serpis.buffer.text() or ""):find(text, 1, true) ~= nil end

if os.getenv("SERPIS_TEST_PTY") then serpis.var.set("terminal-persistent", false) end
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
    serpis.buffer.insert("head -c 5000000 /dev/zero | tr '\\0' x; echo; echo flood-done\r")
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
    when(function() return has("flood-done") end, function(ok)
      T.check("5 MB of output reaches the screen", ok, true)
      times:write(string.format("5 MB: %.2f s\n", clock() - t0))
      times:close()
      T.check("in a few seconds", clock() - t0 < 3.0, true)
      go()
    end)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  steps[i](go)
end
go()
