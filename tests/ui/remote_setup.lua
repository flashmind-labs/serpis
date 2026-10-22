-- A machine with no models or API keys: serpis offers to copy this one's, once the chat is
-- there; the chat then has a model and answers. The machine is a node here in a throwaway home
-- with nothing set up, reached through `serpis-node agent` as ssh would.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local root = os.getenv("SERPIS_TEST_DIR") .. "/../.."
local function sh(cmd) local p = io.popen(cmd); local o = p:read("*a"); p:close(); return o end
local function read(path) local f = io.open(path); if not f then return nil end; local t = f:read("*a"); f:close(); return t end

-- This machine: a model (on the mock provider) with a key.
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s/api/v1","api":"openai-completions","apiKey":"secret-test-key","compat":{"openRouter":true},"models":[{"id":"anthropic/claude-sonnet-4.5"}]}}}', os.getenv("SERPIS_TEST_CACHE_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/anthropic/claude-sonnet-4.5")

-- The other machine: nothing set up.
local box = sh("mktemp -d /tmp/snsetup.XXXXXX"):gsub("%s+$", "")
sh("mkdir -p '" .. box .. "/home/proj' '" .. box .. "/node'")
local home = sh("cd '" .. box .. "/home' && pwd -P"):gsub("%s+$", "")
serpis.var.set("remote-rayfish", false)
serpis.var.set("remote-hosts", { { name = "box", command = { "sh", "-c", "exec env HOME=" .. home .. " SERPIS_AGENT_DIR=" .. home .. "/.serpis/agent SERPIS_NODE_DIR=" .. box .. "/node SERPIS_NODE_SOCKET=" .. box .. "/node/node.sock " .. root .. "/target/debug/serpis-node agent" } } })

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then return T.finish() end
    local wait = list[i](go)
    if wait ~= false then serpis.defer(wait or 400, go) end
  end
  go()
end
local function when(ready, f, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, f, tries + 1) end) end
  serpis.defer(200, f)
end
local function prompt() return d.minibuffer() or "" end
local function said(text)
  for _, m in ipairs(d.messages()) do if m:find(text, 1, true) then return m end end
end

local chat
steps({
  function(go)
    serpis.command.run("remote-new-chat", "box", "~/proj")
    when(function() return prompt():find("has no models", 1, true) ~= nil end, go)
    return false
  end,
  function()
    chat = serpis.buffer.current()
    T.check("a machine with nothing set up gets an offer", prompt(), "box has no models: ")
    local labels = {}
    for _, c in ipairs(d.candidates()) do labels[#labels + 1] = c.label end
    T.check("to copy, not now, or never", table.concat(labels, " | "), "Copy my API keys and models | Not now | Never for this machine")
    T.check("saying what would go", d.candidates()[1].detail:find("mock", 1, true) ~= nil, true)
    T.check("the chat has no model yet", serpis.chat.info(chat).model, nil)
    T.press("RET")
  end,
  function(go)
    when(function() return said("Set up box") ~= nil and serpis.chat.info(chat).model ~= nil end, go)
    return false
  end,
  function(go)
    T.check("it says what it copied", said("Set up box"), "Set up box: API keys for mock, models of mock, the default model")
    T.check("the chat now has the model", serpis.chat.info(chat).model, "mock/anthropic/claude-sonnet-4.5")
    local auth = read(home .. "/.serpis/agent/auth.json") or ""
    T.check("the key is there", auth:find("secret-test-key", 1, true) ~= nil, true)
    T.check("readable by its user only", sh("stat -f %Lp '" .. home .. "/.serpis/agent/auth.json'"):gsub("%s", ""), "600")
    serpis.chat.prompt(chat, "hello")
    when(function() return serpis.chat.transcript(chat):find("ok", 1, true) ~= nil and not serpis.chat.running(chat) end, go)
    return false
  end,
  function(go)
    T.check("and the chat runs there", serpis.chat.transcript(chat):find("ok", 1, true) ~= nil, true)
    -- Asked for again: nothing left to add, and it says so.
    serpis.command.run("remote-setup", "box")
    when(function() return prompt():find("has no models", 1, true) ~= nil end, function()
      T.press("RET")
      when(function() return said("already had all of it") ~= nil end, go)
    end)
    return false
  end,
  function()
    T.check("copying again adds nothing", said("already had all of it"), "box already had all of it")
    serpis.command.run("remote-stop-node", "box")
  end,
  -- The node is really gone before its folder is (or it would outlive the test).
  function(go)
    when(function() return sh("ls '" .. box .. "/node'"):find("node.sock", 1, true) == nil end, function()
      sh("rm -rf '" .. box .. "'")
      go()
    end)
    return false
  end,
})
