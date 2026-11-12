-- Chats' agents come with the report skill and are told to end each task with a report written
-- as it says (chat-report-skill); off, neither.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
local log = os.getenv("SERPIS_TEST_MOCK_LOG")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("remote-rayfish", false)

-- The last request the model got with `text` as its prompt.
local function request(text)
  local found
  local fh = io.open(log)
  if fh then
    for l in fh:lines() do if l:find('"text": "' .. text .. '"', 1, true) then found = l end end
    fh:close()
  end
  return found or ""
end
local function has(req, field) return req:find('"' .. field .. '": true', 1, true) ~= nil end

local steps = {
  function() serpis.command.run("new-chat"); return 600 end,
  function() serpis.chat.prompt(serpis.buffer.current(), "first task"); return 2000 end,
  function()
    local req = request("first task")
    T.check("the model was asked", req ~= "", true)
    T.check("it is offered the report skill", has(req, "report_skill"), true)
    T.check("and told to end each task with a report", has(req, "report_rule"), true)
    local fh = io.open(agent_dir .. "/builtin-skills/report/SKILL.md")
    local text = fh and fh:read("*a") or ""
    if fh then fh:close() end
    T.check("its file is there to read", text:find("name: report", 1, true) ~= nil, true)
    T.check("as serpis has it", text:find("## Needs you", 1, true) ~= nil or text:find("`## Needs you`", 1, true) ~= nil, true)
    serpis.var.set("chat-report-skill", false)
    serpis.command.run("new-chat")
    return 800
  end,
  function() serpis.chat.prompt(serpis.buffer.current(), "second task"); return 2000 end,
  function()
    local req = request("second task")
    T.check("asked again", req ~= "", true)
    T.check("off: no report skill", has(req, "report_skill"), false)
    T.check("and no such rule", has(req, "report_rule"), false)
    T.finish()
    return 100000
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return end
  local ok, wait = pcall(steps[i])
  if not ok then
    T.check("step " .. i, wait, "no error")
    return T.finish()
  end
  serpis.defer(wait or 300, go)
end
go()
