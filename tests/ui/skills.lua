-- Skills on the regular harness: offered to the model in the system prompt, picked with
-- chat-select-skill (C-c /), sent with /skill:name and shown as typed.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "/skills/eng/review'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
f = io.open(agent_dir .. "/skills/eng/review/SKILL.md", "w")
f:write("---\nname: review\ndescription: Review a change. Use when asked to review.\n---\nRead the whole diff first.\n")
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("session-directories", {})
serpis.var.set("remote-rayfish", false)
local log = os.getenv("SERPIS_TEST_MOCK_LOG")
local function last_request()
  local p = io.open(log); local all = p:read("*a"); p:close()
  local line
  for l in all:gmatch("[^\n]+") do line = l end
  return line or ""
end

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 150 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(300, go)
end
local function has(s) return (serpis.chat.transcript() or ""):find(s, 1, true) ~= nil end

local steps = {
  function(go)
    serpis.command.run("new-chat")
    serpis.chat.prompt(nil, "hello")
    when(function() return has("ok") and not serpis.chat.running() end, go)
    return false
  end,
  function(go)
    T.check("the system prompt lists the skills", last_request():find('"skills": true', 1, true) ~= nil, true)
    T.press("C-c /")
    when(function() return d.minibuffer() == "Skill: " end, function()
      d.text("review")
      serpis.defer(300, function()
        T.press("RET")
        when(function() return d.minibuffer() == nil end, go)
      end)
    end)
    return false
  end,
  function(go)
    -- (A chat's text: its transcript, then its composer.)
    T.check("C-c / puts the skill in the composer", (serpis.buffer.text() or ""):sub(-14), "/skill:review ")
    d.text("the last commit")
    T.press("RET")
    when(function() return has("/skill:review the last commit") and not serpis.chat.running() end, go)
    return false
  end,
  function()
    local req = last_request()
    T.check("its instructions are sent", req:find('<skill name=\\"review\\"', 1, true) ~= nil and req:find("Read the whole diff first.", 1, true) ~= nil, true)
    T.check("with what was typed after it", req:find("the last commit", 1, true) ~= nil, true)
    T.check("the transcript shows it as typed", has("/skill:review the last commit") and not has("Read the whole diff"), true)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(300, go) end
end
go()
