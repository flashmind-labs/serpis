-- An answer's usage line starts with how long the response took, from sending the message to
-- the answer's end (its tool calls included); chat-response-time hides it, and a resumed chat
-- shows the time it was saved with.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
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
local chat = function() return serpis.buffer.current() end
local function transcript() return serpis.chat.transcript() or "" end
local function row(text) return ("\n" .. transcript() .. "\n"):find("\n" .. text .. "\n", 1, true) ~= nil end
-- The usage line's leading time in seconds ("↳ 1.4s · 1k in …"), or nil.
local function took() return tonumber(transcript():match("↳ (%d+%.?%d*)s · ")) end
local function usage_line() return transcript():match("↳ [^\n]*") end

local path, first
local steps = {
  function(go)
    serpis.command.run("new-chat")
    -- The tool call takes a second and a half; the answer after it counts it.
    serpis.chat.prompt(chat(), "SHELL:sleep 1.5")
    when(function() return row("done") and not serpis.chat.running(chat()) end, go)
  end,
  function(go)
    first = took()
    T.check("the answer's usage line starts with its time", first ~= nil, true)
    T.check("the time covers the tool call", first ~= nil and first >= 1.5 and first < 15, true)
    T.check("token counts follow it", usage_line() and usage_line():find("s · 1.0k in · ", 1, true) ~= nil, true)
    serpis.var.set("chat-response-time", false)
    serpis.defer(300, go)
  end,
  function(go)
    T.check("chat-response-time off: no time", took(), nil)
    T.check("chat-response-time off: counts stay", usage_line() and usage_line():find("↳ 1.0k in", 1, true) ~= nil, true)
    serpis.var.set("chat-response-time", true)
    path = serpis.session.of(chat())
    serpis.buffer.kill(chat())
    serpis.defer(500, go)
  end,
  function(go)
    serpis.session.resume(path)
    when(function() return row("done") end, go)
  end,
  function(go)
    T.check("a resumed chat shows the saved time", took(), first)
    T.finish()
  end,
}

local i = 0
local function go()
  i = i + 1
  if steps[i] then steps[i](go) end
end
go()
