-- codex-status: the stand-in codex's (serpis-fake-codex) account, limits and activity, in a page
-- of cards like claude-code-usage's; g asks again, q closes it.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.var.set("remote-rayfish", false)
serpis.var.set("codex-path", os.getenv("SERPIS_TEST_DIR") .. "/../../target/debug/serpis-fake-codex")

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 200 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(200, go)
end
local function page() return serpis.buffer.text(serpis.buffer.current()) or "" end
local function shown() return page():find("Pro plan", 1, true) ~= nil end

local steps = {
  function(go)
    T.press("M-x")
    when(function() return d.minibuffer() ~= nil end, function()
      d.text("codex-status")
      serpis.defer(300, function()
        T.press("RET")
        when(shown, go)
      end)
    end)
    return false
  end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if not shots then return go() end
    -- In a narrow window (beside the chat), then filling the frame.
    serpis.defer(1000, function()
      d.screenshot(shots .. "/codex-status.png")
      serpis.command.run("delete-other-windows")
      serpis.defer(800, function() d.screenshot(shots .. "/codex-status-wide.png"); serpis.defer(500, go) end)
    end)
    return false
  end,
  function(go)
    local text = page()
    T.check("codex-status shows the status page", serpis.buffer.name(serpis.buffer.current()), "*codex-status*")
    T.check("with the plan", text:find("Codex status · Pro plan", 1, true) ~= nil, true)
    T.check("the account", text:find("Account: codex@example.test", 1, true) ~= nil, true)
    T.check("the 5-hour limit", text:find("5-hour limit: 25% used, 75% left", 1, true) ~= nil, true)
    T.check("the weekly limit", text:find("Weekly limit: 40% used", 1, true) ~= nil, true)
    T.check("and the activity", text:find("Lifetime tokens: 1.2M", 1, true) ~= nil, true)
    -- g asks again, in place.
    T.press("g")
    when(shown, function()
      T.check("in the same page", serpis.buffer.name(serpis.buffer.current()), "*codex-status*")
      T.press("q")
      serpis.defer(300, function()
        T.check("q closes it", serpis.buffer.name(serpis.buffer.current()) ~= "*codex-status*", true)
        go()
      end)
    end)
    return false
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(400, go) end
end
go()
