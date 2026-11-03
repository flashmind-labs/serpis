-- The *Log* page: what serpis logs (scripts' serpis.log, messages) and what this machine's node
-- logs, filtered by level and text with its keys and controls.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.var.set("remote-rayfish", false)

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(200, go)
end
local function page() return serpis.buffer.text(serpis.buffer.current()) or "" end
local function has(s) return page():find(s, 1, true) ~= nil end
local function click(name)
  local r = d.composer()[name]
  if not r then return false end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  serpis.defer(80, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
  return true
end

local steps = {
  function(go)
    serpis.log.info("hello from lua", { n = 3, who = "test" })
    serpis.log.warn("careful now")
    serpis.log.debug("not kept at info")
    serpis.message("a message too")
    T.press("M-x")
    when(function() return d.minibuffer() ~= nil end, function()
      d.text("log-view")
      serpis.defer(300, function()
        T.press("RET")
        when(function() return serpis.buffer.name(serpis.buffer.current()) == "*Log*" and has("careful now") end, go)
      end)
    end)
    return false
  end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then
      serpis.defer(800, function() d.screenshot(shots .. "/log-view.png"); serpis.defer(300, go) end)
      return false
    end
  end,
  function(go)
    -- Given the whole window, it lays out in columns.
    serpis.command.run("delete-other-windows")
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if shots then
      serpis.defer(800, function() d.screenshot(shots .. "/log-view-wide.png"); serpis.defer(300, go) end)
      return false
    end
  end,
  function()
    T.check("log-view opens the *Log* page", serpis.buffer.name(serpis.buffer.current()), "*Log*")
    T.check("a script's record, with its fields", page():match("INFO  lua: hello from lua · n=3 · who=test") ~= nil, true)
    T.check("its warning", page():match("WARN  lua: careful now") ~= nil, true)
    T.check("messages are in it", page():match("INFO  message: a message too") ~= nil, true)
    T.check("debug isn't kept at log-level info", has("not kept at info"), false)
    local recs = serpis.log.records()
    local found = false
    for _, r in ipairs(recs) do
      if r.target == "lua" and r.message == "hello from lua" then found = r.level == "info" end
    end
    T.check("serpis.log.records reads them back", found, true)
    T.check("its controls are drawn", d.composer()["level-warn"] ~= nil and d.composer()["search"] ~= nil, true)
  end,
  function(go)
    -- Warnings and up, clicked.
    click("level-warn")
    when(function() return not has("hello from lua") end, go)
    return false
  end,
  function(go)
    T.check("the Warnings control hides info", has("hello from lua"), false)
    T.check("and keeps warnings", has("careful now"), true)
    -- l goes on to the next level (debug); at info again, / searches.
    T.press("l")
    serpis.defer(200, function()
      T.press("l")
      when(function() return has("hello from lua") end, go)
    end)
    return false
  end,
  function(go)
    T.check("l cycles the level shown", has("hello from lua"), true)
    T.press("/")
    when(function() return d.minibuffer() == "Search the log: " end, function()
      d.text("hello")
      serpis.defer(200, function()
        T.press("RET")
        when(function() return not has("careful now") end, go)
      end)
    end)
    return false
  end,
  function(go)
    T.check("/ shows only what matches", has("hello from lua") and not has("careful now"), true)
    -- Clicking the search pill again clears it.
    click("search")
    when(function() return has("careful now") end, go)
    return false
  end,
  function(go)
    T.check("and its ✕ shows all again", has("careful now"), true)
    -- Debug records are kept once log-level says so.
    serpis.var.set("log-level", "debug")
    serpis.log.debug("kept at debug")
    serpis.command.run("log-cycle-level", "debug")
    when(function() return has("kept at debug") end, go)
    return false
  end,
  function(go)
    T.check("log-level debug keeps debug records", has("kept at debug"), true)
    serpis.var.set("log-level", "info")
    -- This machine's node's log: its own source, once a chat started it.
    serpis.command.run("new-chat")
    serpis.defer(1500, function()
      serpis.command.run("log-view")
      when(function() return d.composer()["source-1"] ~= nil end, function()
        T.press("s")
        when(function() return has("listening on") end, go)
      end)
    end)
    return false
  end,
  function(go)
    T.check("s shows this machine's node's log", has("serpis-node") and has("listening on"), true)
    T.press("s")
    when(function() return has("careful now") end, go)
    return false
  end,
  function(go)
    -- Selecting: a drag over one record's text.
    serpis.command.run("log-cycle-level", "info")
    when(function() return d.composer()["text:careful now"] ~= nil and d.composer()["text:hello from lua"] ~= nil end, function()
      local r = d.composer()["text:careful now"]
      local y = r.y + r.h / 2
      d.mouse_move(r.x + 1, y)
      serpis.defer(60, function()
        d.mouse_button(r.x + 1, y, "left", true)
        serpis.defer(60, function()
          d.mouse_move(r.x + r.w / 2, y)
          serpis.defer(60, function()
            d.mouse_move(r.x + r.w + 60, y)
            serpis.defer(100, function()
              d.mouse_button(r.x + r.w + 60, y, "left", false)
              serpis.defer(100, go)
            end)
          end)
        end)
      end)
    end)
    return false
  end,
  function(go)
    T.check("a drag selects what a record says", d.log_selected(), "careful now")
    -- The key that copies it, as the keyboard sends it.
    T.press("y")
    serpis.defer(150, function()
      local ms = d.messages()
      T.check("y copies it", ms[#ms], "Copied")
      -- A click lets go of it, without opening the record.
      local r = d.composer()["text:careful now"]
      local h = r.h
      local x, y = r.x + 5, r.y + r.h / 2
      d.mouse_move(x, y)
      serpis.defer(60, function()
        d.mouse_button(x, y, "left", true)
        d.mouse_button(x, y, "left", false)
        serpis.defer(200, function()
          T.check("a click clears the selection", d.log_selected(), nil)
          T.check("and doesn't open the record", d.composer()["text:careful now"].h, h)
          -- Across records, from the middle of one to the middle of the next.
          local a, b = d.composer()["text:hello from lua"], d.composer()["text:careful now"]
          local cw = a.w / #"hello from lua"
          local from_x, from_y = a.x + cw * 6 + 1, a.y + a.h / 2
          local to_x, to_y = b.x + cw * 7 + 1, b.y + b.h / 2
          d.mouse_move(from_x, from_y)
          serpis.defer(60, function()
            d.mouse_button(from_x, from_y, "left", true)
            serpis.defer(60, function()
              d.mouse_move(from_x + 20, from_y + 2)
              serpis.defer(60, function()
                d.mouse_move(to_x, to_y)
                serpis.defer(100, function()
                  d.mouse_button(to_x, to_y, "left", false)
                  serpis.defer(100, go)
                end)
              end)
            end)
          end)
        end)
      end)
    end)
    return false
  end,
  function(go)
    T.check("a drag across records takes them all", d.log_selected(), "from lua\nn = 3\nwho = test\ncareful")
    -- A click with nothing selected still opens a record.
    local r = d.composer()["text:hello from lua"]
    local x, y = r.x + 5, r.y + r.h / 2
    d.mouse_move(x, y)
    serpis.defer(60, function()
      d.mouse_button(x, y, "left", true)
      d.mouse_button(x, y, "left", false)
      serpis.defer(200, function()
        T.check("that click only let go", d.log_selected(), nil)
        d.mouse_button(x, y, "left", true)
        d.mouse_button(x, y, "left", false)
        serpis.defer(200, function()
          T.check("the next click opens the record", d.composer()["text:hello from lua"].h > r.h, true)
          go()
        end)
      end)
    end)
    return false
  end,
  function(go)
    -- Clear empties it; new records still come.
    click("clear")
    when(function() return not has("careful now") end, function()
      serpis.log.info("after the clear")
      when(function() return has("after the clear") end, go)
    end)
    return false
  end,
  function()
    T.check("Clear empties the page", has("careful now"), false)
    T.check("and new records still come", has("after the clear"), true)
    T.press("q")
  end,
  function()
    T.check("q closes it", serpis.buffer.name(serpis.buffer.current()) ~= "*Log*", true)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(300, go) end
end
go()
