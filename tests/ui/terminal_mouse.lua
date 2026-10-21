-- Selecting terminal text with the mouse, as in other terminals: drag selects characters,
-- double-click a word (paths whole), triple-click the line, a click clears; what is selected
-- is what copying takes (terminal-copy, Cmd-C).
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.command.run("new-terminal")

local function when(ready, f, tries)
  tries = tries or 150
  if ready() or tries == 0 then return f() end
  serpis.defer(100, function() when(ready, f, tries - 1) end)
end
local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if list[i] then list[i](go) else T.finish() end
  end
  go()
end

-- The output line (1-based) and where a column of it is on screen.
local OUT = "alpha beta /tmp/x-y.txt gamma"
local function out_line()
  local n = 0
  for l in (serpis.buffer.text() .. "\n"):gmatch("(.-)\n") do
    n = n + 1
    if l == OUT then return n end
  end
end
local function at(col)
  local r = d.cell_rect(out_line(), col)
  return r.x + r.w / 2, r.y + r.h / 2
end
-- Real input: the pointer arrives, then presses (`clicks` times), optionally drags, releases.
local function mouse(from, to, clicks, done)
  local x, y = at(from)
  d.mouse_move(x, y)
  local function click(n)
    serpis.defer(60, function()
      d.mouse_button(x, y, "left", true)
      serpis.defer(40, function()
        if n == 1 and to then
          local x2, y2 = at(to)
          d.mouse_move(x2, y2)
          serpis.defer(60, function()
            d.mouse_button(x2, y2, "left", false)
            serpis.defer(150, done)
          end)
        else
          d.mouse_button(x, y, "left", false)
          if n > 1 then click(n - 1) else serpis.defer(150, done) end
        end
      end)
    end)
  end
  click(clicks)
end

when(function() return serpis.buffer.text():find("%S") ~= nil end, function() serpis.defer(700, function()
  serpis.buffer.insert("clear; echo '" .. OUT .. "'\r")
  when(function() return out_line() ~= nil end, function() serpis.defer(300, function()
    steps({
      function(go) mouse(7, 10, 1, go) end,
      function(go)
        T.check("drag selects characters", d.selection(), "beta")
        mouse(15, nil, 2, go)
      end,
      function(go)
        T.check("double-click selects a word (a path whole)", d.selection(), "/tmp/x-y.txt")
        if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/terminal-select.png") end
        mouse(3, nil, 3, go)
      end,
      function(go)
        T.check("triple-click selects the line", d.selection(), OUT)
        -- After the triple-click's timeout, a single click clears it.
        serpis.defer(500, function() mouse(3, nil, 1, go) end)
      end,
      function(go)
        T.check("a click clears it", d.selection(), nil)
        mouse(1, 5, 1, function()
          T.check("selected again", d.selection(), "alpha")
          serpis.buffer.insert("x")
          serpis.defer(200, go)
        end)
      end,
      function(go)
        T.check("typing clears it", d.selection(), nil)
        go()
      end,
    })
  end) end)
end) end)
