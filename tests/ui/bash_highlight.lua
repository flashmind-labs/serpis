-- A bash tool call's card: the command highlighted as shell (the command it runs, its flags,
-- quotes, $variables and operators each in a color), its later lines too, then what it printed.
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
local function has(text) return (serpis.chat.transcript() or ""):find(text, 1, true) ~= nil end

local command = table.concat({
  [[NAME="serpis tests" && echo "hello, $NAME" | grep -E 'hello|bye' --color=never 2>&1 # say it]],
  [[for n in 1 2; do]],
  [[  printf '%s\n' "line ${n}" > /dev/null]],
  [[done; echo $(echo nested) ok]],
}, "\n")

local chat
local steps = {
  function(go)
    serpis.command.run("new-chat")
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "SHELL:" .. command)
    when(function() return has("nested ok") and not serpis.chat.running(chat) end, go)
    return false
  end,
  function(go)
    local shots = os.getenv("SERPIS_TEST_SHOTS")
    if not shots then return go() end
    serpis.defer(800, function() d.screenshot(shots .. "/bash-highlight.png"); serpis.defer(500, go) end)
    return false
  end,
  function()
    T.check("the card shows the command's first line", has([[echo "hello, $NAME" | grep -E 'hello|bye']]), true)
    T.check("and its later lines", has([[printf '%s\n' "line ${n}" > /dev/null]]), true)
    T.check("and what it printed", has("hello, serpis tests"), true)
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return T.finish() end
  if steps[i](go) ~= false then serpis.defer(400, go) end
end
go()
