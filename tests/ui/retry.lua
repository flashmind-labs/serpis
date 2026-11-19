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
serpis.var.set("agent-max-retries", 0)

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(300, go)
end
local function idle() return serpis.buffer.activity().state ~= "working" end
local function has(text) return (serpis.chat.transcript() or ""):find(text, 1, true) ~= nil end
local function count(text)
  local n, from = 0, 1
  local t = serpis.chat.transcript() or ""
  while true do
    local i = t:find(text, from, true)
    if not i then return n end
    n, from = n + 1, i + #text
  end
end
local function oks()
  local n = 0
  for _ in (serpis.chat.transcript() or ""):gmatch("%f[%w]ok%f[%W]") do n = n + 1 end
  return n
end
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
  T.press("B U S Y SPC o n e RET")
  when(function() return idle() and has("Rate limit exceeded") end, function()
    T.check("a rate limit past its retries fails the turn", has("Rate limit exceeded"), true)
    T.check("a failed turn can be retried", serpis.chat.can_retry(), true)
    shot("retry-button")
    click("send")
    when(function() return idle() and oks() == 1 end, function()
      T.check("the replay button sends the turn again", oks(), 1)
      T.check("the prompt isn't sent twice", count("BUSY one"), 1)
      T.check("an answered turn has nothing to retry", serpis.chat.can_retry(), false)
      T.press("B U S Y SPC t w o RET")
      when(function() return idle() and count("Rate limit exceeded") == 2 end, function()
        T.press("RET")
        when(function() return idle() and oks() == 2 end, function()
          T.check("RET in an empty composer sends the failed turn again", oks(), 2)
          T.check("still one prompt", count("BUSY two"), 1)
          T.finish()
        end)
      end)
    end)
  end)
end)
