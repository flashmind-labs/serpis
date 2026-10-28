-- A chat whose node is killed mid-turn (as a crash or a reboot does it) keeps going: the window
-- connects again, the node starts again, and the turn continues, saying so; the tool that was
-- cut off isn't run again. The composer's pill says whether the chat keeps going, and a click
-- on it changes that.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
T.room()
local d = serpis.debug
local function sh(cmd) local p = io.popen(cmd); local o = p:read("*a"); p:close(); return o end

local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s/api/v1","api":"openai-completions","apiKey":"x","compat":{"openRouter":true},'
  .. '"models":[{"id":"anthropic/claude-sonnet-4.5","contextWindow":200000}]}}}', os.getenv("SERPIS_TEST_CACHE_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/anthropic/claude-sonnet-4.5")
serpis.var.set("remote-rayfish", false)

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 200 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(200, go)
end
local function has(text) return (serpis.chat.transcript() or ""):find(text, 1, true) ~= nil end
local function click(name)
  local r = d.composer()[name]
  if not r then return end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
end

local steps = {
  function(go)
    serpis.command.run("new-chat")
    when(function() return d.composer()["keep-going"] ~= nil end, go)
    return false
  end,
  function(go)
    T.check("a chat keeps going by default", serpis.chat.info().keep_going, true)
    click("keep-going")
    when(function() return serpis.chat.info().keep_going == false end, go)
    return false
  end,
  function(go)
    T.check("clicked, it waits", serpis.chat.info().keep_going, false)
    click("keep-going")
    when(function() return serpis.chat.info().keep_going == true end, go)
    return false
  end,
  function(go)
    T.check("clicked again, it keeps going", serpis.chat.info().keep_going, true)
    d.text('CALL bash {"command": "sleep 30"}')
    T.press("RET")
    when(function() return serpis.chat.running() and has("sleep 30") end, go)
    return false
  end,
  function(go)
    -- Killed, as a crash would.
    local pid = sh("cat \"$HOME/.serpis/node/node.pid\""):gsub("%s+$", "")
    T.check("the node runs the chat", pid ~= "", true)
    os.execute("kill -9 " .. pid)
    when(function() return has("Continued") and not serpis.chat.running() end, go)
    return false
  end,
  function()
    T.check("the turn continued", has("Continued"), true)
    T.check("the cut-off tool wasn't run again", has("Not run to the end"), true)
    T.check("and the turn finished", has("done"), true)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(400, go) end
end
go()
