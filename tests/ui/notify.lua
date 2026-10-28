-- Notifications (notify-backend "none" in tests: recorded, not shown): a chat's turn that ran a
-- while, only when not looking (notify-when), a terminal's command, scripts' own, and the
-- notify hook changing or dropping them.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("session-directories", {})
serpis.var.set("remote-rayfish", false)

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 150 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(300, go)
end
local function sent() return d.notifications() or {} end
local function last() local s = sent(); return s[#s] or {} end

local chat, before
local steps = {
  function(go)
    T.check("tests don't show them on the desktop", serpis.var.get("notify-backend"), "none")
    T.check("they notify whether you look or not, by default", serpis.var.get("notify-when"), "always")
    serpis.var.set("notify-after", 0)
    serpis.var.set("notify-when", "always")
    serpis.command.run("new-chat")
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "hello")
    when(function() return #sent() > 0 and not serpis.chat.running(chat) end, go)
    return false
  end,
  function(go)
    local n = last()
    T.check("a chat's turn ending notifies", n.event, "done")
    T.check("titled by the chat", n.title, "hello")
    T.check("with how long and what it said", (n.body or ""):match("^Done in .+: ok$") ~= nil, true)
    T.check("and which buffer a click opens", n.buffer, chat)
    T.check("it is logged too", (function()
      for _, r in ipairs(serpis.log.records()) do
        if r.target == "serpis::notify" and r.message:find("Done in", 1, true) then return true end
      end
      return false
    end)(), true)
    -- Too short a turn (notify-after) doesn't.
    serpis.var.set("notify-after", 600)
    before = #sent()
    serpis.chat.prompt(chat, "again")
    when(function() return not serpis.chat.running(chat) end, go)
    return false
  end,
  function(go)
    T.check("a turn shorter than notify-after doesn't notify", #sent(), before)
    -- Looking at it (notify-when unseen, the chat on screen and focused) doesn't either.
    serpis.var.set("notify-after", 0)
    serpis.var.set("notify-when", "unseen")
    serpis.chat.prompt(chat, "once more")
    when(function() return not serpis.chat.running(chat) end, go)
    return false
  end,
  function(go)
    -- (Whether the test's window has the system's focus is up to the system: either way, it is
    -- exact.)
    if d.focused() then
      T.check("nor one you are looking at", #sent(), before)
    else
      T.check("one on screen still does while serpis isn't the focused app", #sent(), before + 1)
    end
    serpis.var.set("notify-when", "always")
    -- The hook changes one, or drops it.
    serpis.hook.add("notify", function(p)
      if p.body:find("drop me", 1, true) then return false end
      if p.event == "script" then return { title = "[changed] " .. p.title } end
    end)
    before = #sent()
    serpis.notify("Built", "drop me")
    serpis.notify("Built", "the release is ready", { buffer = chat })
    serpis.defer(300, go)
    return false
  end,
  function(go)
    T.check("false from the hook drops it", #sent(), before + 1)
    T.check("serpis.notify sends one, as the hook changed it", last().title, "[changed] Built")
    T.check("with its body", last().body, "the release is ready")
    -- A terminal's command finishing.
    serpis.command.run("new-terminal")
    local term = serpis.buffer.current()
    when(function() return (serpis.buffer.text(term) or ""):find("%S") ~= nil end, function()
      serpis.defer(700, function()
        before = #sent()
        serpis.buffer.insert("sleep 1\r")
        when(function() return #sent() > before end, go)
      end)
    end)
    return false
  end,
  function(go)
    T.check("a terminal's command finishing notifies", (last().body or ""):match("^Command finished in") ~= nil, true)
    T.check("as done", last().event, "done")
    -- notify off: nothing.
    serpis.var.set("notify", false)
    before = #sent()
    serpis.chat.prompt(chat, "quiet")
    when(function() return not serpis.chat.running(chat) end, go)
    return false
  end,
  function()
    T.check("notify false sends none", #sent(), before)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(300, go) end
end
go()
