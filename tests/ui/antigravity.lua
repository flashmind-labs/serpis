-- A chat on Antigravity (the stand-in agy, serpis-fake-agy): model picker, streaming reply,
-- permission prompts (Allow/Always), inline edit diffs, S-TAB permission-mode cycling, and
-- M-x antigravity-usage (g refreshes, q closes).
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
serpis.var.set("remote-rayfish", false)

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 200 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(200, go)
end
local function has(text) return (serpis.chat.transcript() or ""):find(text, 1, true) ~= nil end
local function asking() return d.composer()["allow"] ~= nil end
local function idle_with(text) return function() return has(text) and not serpis.chat.running() and not asking() end end
local function page() return serpis.buffer.text(serpis.buffer.current()) or "" end

local steps = {
  function(go)
    when(function() return serpis.agent.find_model("antigravity/gemini-4-neon") ~= nil end, go)
    return false
  end,
  function(go)
    T.check("Antigravity models are offered", serpis.agent.find_model("antigravity/gemini-4-neon") ~= nil, true)
    serpis.command.run("new-chat")
    when(function() return d.composer()["send"] ~= nil end, go)
    return false
  end,
  function(go)
    T.press("C-c C-m")
    when(function() return d.minibuffer() == "Model: " end, function()
      d.text("antigravity/gemini-4-neon")
      serpis.defer(500, function()
        T.press("RET")
        when(function() return (serpis.chat.info() or {}).model == "antigravity/gemini-4-neon" end, go)
      end)
    end)
    return false
  end,
  function(go)
    T.check("chat-select-model offers antigravity/* and the chat takes it", (serpis.chat.info() or {}).model, "antigravity/gemini-4-neon")
    d.text("hello antigravity")
    T.press("RET")
    when(idle_with("echo: hello antigravity"), go)
    return false
  end,
  function(go)
    T.check("Antigravity streams replies", has("echo: hello antigravity"), true)
    T.check("the composer shows permission mode", d.composer()["permission-mode"] ~= nil, true)
    -- Shift+Tab cycles Antigravity's modes.
    local function mode() return (serpis.chat.info() or {}).permission_mode end
    T.press("S-TAB")
    when(function() return mode() == "accept-edits" end, function()
      T.check("S-TAB: accept-edits", mode(), "accept-edits")
      T.press("S-TAB")
      when(function() return mode() == "auto" end, function()
        T.check("S-TAB: auto", mode(), "auto")
        T.press("S-TAB")
        when(function() return mode() == "plan" end, function()
          T.check("S-TAB: plan", mode(), "plan")
          T.press("S-TAB")
          when(function() return mode() == "default" end, go)
        end)
      end)
    end)
    return false
  end,
  function(go)
    T.check("S-TAB: back to default", (serpis.chat.info() or {}).permission_mode, "default")
    d.text("run a command")
    T.press("RET")
    when(asking, go)
    return false
  end,
  function(go)
    T.check("permission prompt waits for approval", asking(), true)
    T.press("C-c C-y")
    when(idle_with("Command finished."), go)
    return false
  end,
  function(go)
    T.check("C-c C-y allowed the command", has("Command finished."), true)
    d.text("edit a file")
    T.press("RET")
    when(idle_with("Edited notes.txt."), go)
    return false
  end,
  function(go)
    T.check("inline diff renders on edit card", has("-1 before") and has("+1 after"), true)
    -- M-x antigravity-usage opens *antigravity-usage*, g refreshes, q closes.
    T.press("M-x")
    when(function() return d.minibuffer() ~= nil end, function()
      d.text("antigravity-usage")
      serpis.defer(300, function()
        T.press("RET")
        when(function() return page():find("Gemini Models", 1, true) ~= nil end, go)
      end)
    end)
    return false
  end,
  function(go)
    local text = page()
    T.check("antigravity-usage shows the usage page", serpis.buffer.name(serpis.buffer.current()), "*antigravity-usage*")
    T.check("with model groups", text:find("Gemini Models", 1, true) ~= nil, true)
    T.check("and weekly window", text:find("Weekly window: 15% used, 85% left", 1, true) ~= nil, true)
    T.press("g")
    when(function() return page():find("Gemini Models", 1, true) ~= nil end, function()
      T.check("g refreshes in place", serpis.buffer.name(serpis.buffer.current()), "*antigravity-usage*")
      T.press("q")
      serpis.defer(300, function()
        T.check("q closes it", serpis.buffer.name(serpis.buffer.current()) ~= "*antigravity-usage*", true)
        go()
      end)
    end)
    return false
  end,
  function(go)
    serpis.agent.antigravity_usage(function(u, err)
      T.check("serpis.agent.antigravity_usage answers", u and u.groups[1].name, "Gemini Models")
      T.check("and no error", err, nil)
      go()
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
