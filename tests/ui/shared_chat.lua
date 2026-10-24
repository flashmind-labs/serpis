-- A chat started in this window runs in this machine's node, so another device that reaches
-- this machine finds it and drives it: here "phone" is this machine's own node reached the way
-- another machine reaches it (`serpis-node agent`, as over ssh), on a connection of its own. Its
-- session list says the chat is running; picked, it shows the same live chat, and what is sent
-- from either side shows on both.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd) local p = io.popen(cmd); local o = p:read("*a"); p:close(); return o end

local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s/api/v1","api":"openai-completions","apiKey":"x","compat":{"openRouter":true},'
  .. '"models":[{"id":"anthropic/claude-sonnet-4.5","contextWindow":200000}]}}}', os.getenv("SERPIS_TEST_CACHE_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/anthropic/claude-sonnet-4.5")
serpis.var.set("session-directories", {})
serpis.var.set("remote-rayfish", false)
serpis.var.set("remote-offer-setup", false)
serpis.var.set("remote-hosts", { { name = "phone", command = { root .. "/target/debug/serpis-node", "agent" } } })

local function when(ready, f, tries)
  tries = tries or 0
  if not ready() and tries < 150 then return serpis.defer(100, function() when(ready, f, tries + 1) end) end
  serpis.defer(200, f)
end
local function has(buf, text) return (serpis.chat.transcript(buf) or ""):find(text, 1, true) ~= nil end
local function row(where)
  for _, c in ipairs(d.candidates() or {}) do
    if c.label:find("CALL bash", 1, true) and c.label:find(where, 1, true) then return c end
  end
end

local here, there, id
local steps = {
  function(go)
    serpis.command.run("new-chat")
    here = serpis.buffer.current()
    -- (The mock model runs it as asked: the chat works for a while.)
    d.text('CALL bash {"command": "sleep 8"}')
    T.press("RET")
    when(function() return serpis.chat.running(here) end, go)
    return false
  end,
  function()
    id = serpis.chat.info(here).session_id
    T.check("the chat works in this machine's node", serpis.chat.running(here), true)
    serpis.var.set("chat-resume-scope", "everywhere")
    serpis.command.run("chat-resume")
  end,
  function(go)
    when(function() return not serpis.minibuffer.loading() and row("on phone") ~= nil and row("here") ~= nil end, go)
    return false
  end,
  function(go)
    local r = row("on phone")
    T.check("another device lists it", r ~= nil, true)
    T.check("as running", r and r.detail:find("running", 1, true) ~= nil, true)
    T.check("here too", row("here") and row("here").detail:find("running", 1, true) ~= nil, true)
    T.press("p h o n e RET")
    when(function() return serpis.buffer.current() ~= here and serpis.chat.info() and serpis.chat.info().machine == "phone" and serpis.chat.info().title ~= "" end, go)
    return false
  end,
  function(go)
    there = serpis.buffer.current()
    T.check("picked, it is the same chat", serpis.chat.info(there).session_id, id)
    T.check("live: still working", serpis.chat.running(there), true)
    -- It finishes for both.
    when(function() return not serpis.chat.running(there) and not serpis.chat.running(here) and has(here, "done") end, go)
    return false
  end,
  function(go)
    T.check("both saw it finish", has(there, "done") and has(here, "done"), true)
    -- Driven from the other device, seen here.
    T.press("h e l l o SPC f r o m SPC t h e r e RET")
    when(function() return has(here, "hello from there") and not serpis.chat.running(here) end, go)
    return false
  end,
  function(go)
    T.check("what the other device sends shows here", has(here, "hello from there"), true)
    serpis.buffer.show(here)
    T.press("a n d SPC b a c k RET")
    when(function() return has(there, "and back") and not serpis.chat.running(there) end, go)
    return false
  end,
  function()
    T.check("and the other way", has(there, "and back"), true)
    T.check("one transcript", serpis.chat.transcript(here) == serpis.chat.transcript(there), true)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  -- A step that returns false calls go itself; the rest go on after a moment.
  if steps[i](go) ~= false then serpis.defer(400, go) end
end
go()
