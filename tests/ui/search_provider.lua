-- A web search's card, opened, says which provider its results came from and which failed
-- before it (the model isn't told). Exa's free endpoint is a closed port here and DuckDuckGo
-- the mock (ui-test.sh): Exa fails, DuckDuckGo answers. Nothing leaves the machine.
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
local function transcript() return serpis.chat.transcript() or "" end
local function has(text) return transcript():find(text, 1, true) ~= nil end
local function idle() return serpis.buffer.activity().state ~= "working" end
local function click_row(text)
  local n = 1
  for l in (transcript() .. "\n"):gmatch("(.-)\n") do
    if l:find(text, 1, true) then break end
    n = n + 1
  end
  local r = d.cell_rect(n, 0)
  d.mouse_move(r.x + 4, r.y + 4)
  d.mouse_button(r.x + 4, r.y + 4, "left", true)
  d.mouse_button(r.x + 4, r.y + 4, "left", false)
end

serpis.command.run("new-chat")
serpis.defer(600, function()
  T.press("S E A R C H : r u s t RET")
  when(function() return idle() and has("done") end, function()
    T.check("closed, the card doesn't say", has("via duckduckgo"), false)
    serpis.command.run("chat-focus-transcript")
    serpis.defer(800, function()
    click_row("web_search")
    serpis.defer(500, function()
      T.check("opened, it says which provider answered", has("via duckduckgo"), true)
      T.check("and which failed first", has("exa failed first:"), true)
      T.check("then the results", has("Rust results"), true)
      if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/search-provider.png") end
      -- The order set (agent-web-search-providers): only DuckDuckGo, so Exa isn't tried.
      serpis.var.set("agent-web-search-providers", { "duckduckgo" })
      serpis.command.run("new-chat")
      serpis.defer(600, function()
        T.press("S E A R C H : r u s t RET")
        when(function() return idle() and has("done") end, function()
          serpis.command.run("chat-focus-transcript")
          serpis.defer(800, function()
            click_row("web_search")
            serpis.defer(500, function()
              T.check("with the order set, its providers answer", has("via duckduckgo"), true)
              T.check("and no other is tried", has("exa failed first"), false)
              T.finish()
            end)
          end)
        end)
      end)
    end)
    end)
  end)
end)
