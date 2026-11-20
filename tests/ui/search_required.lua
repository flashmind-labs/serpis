-- Every web search provider failing stops the run (agent-web-search-required), and the turn
-- can be sent again from the composer, as a rate-limited one can. DuckDuckGo is the mock here
-- (ui-test.sh), which fails every search: nothing leaves the machine.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
T.room()
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("session-directories", {})

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(300, go)
end
local function idle() return serpis.buffer.activity().state ~= "working" end
local function has(text) return (serpis.chat.transcript() or ""):find(text, 1, true) ~= nil end
local function click(name)
  local r = d.composer()[name]
  if not r then return end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  d.mouse_button(x, y, "left", true)
  d.mouse_button(x, y, "left", false)
end
local function shot(name)
  if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/" .. name .. ".png") end
end

serpis.command.run("new-chat")
serpis.defer(600, function()
  T.press("S E A R C H : n e w s RET")
  when(function() return idle() and has("Web search failed") end, function()
    T.check("every provider failing stops the run", has("Web search failed with every provider"), true)
    T.check("the model wasn't asked to go on without search", has("done"), false)
    T.check("the stopped turn can be retried", serpis.chat.can_retry(), true)
    shot("search-stopped")
    click("send")
    when(function() return idle() and has("done") end, function()
      T.check("the replay button sends the turn on from the failed search", has("done"), true)
      T.check("a turn that went on has nothing to retry", serpis.chat.can_retry(), false)
      T.finish()
    end)
  end)
end)
