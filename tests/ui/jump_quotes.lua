-- ninja-copy-quoted (s-g q) offers backtick strings too: typed ones keep their backticks, and a
-- chat shows inline code without them, as code chips, which are offered as well.
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

serpis.command.run("new-chat")
local chat = serpis.buffer.current()
serpis.chat.prompt(chat, "INLINE please")
local function wait(fn, n)
  n = n or 0
  if serpis.chat.running(chat) and n < 100 then return serpis.defer(100, function() wait(fn, n + 1) end) end
  serpis.defer(400, fn)
end
wait(function()
  d.text("try `make all` or 'x'")
  serpis.defer(300, function()
    T.press("s-g q")
    serpis.defer(300, function()
      local texts = {}
      for _, l in ipairs(serpis.jump.labels()) do
        if l.text then texts[l.text] = true end
      end
      T.check("a chat's inline code is offered", texts["cargo test"], true)
      T.check("typed backtick strings are offered", texts["make all"], true)
      T.check("single quotes still are", texts["x"], true)
      serpis.jump.cancel()
      T.finish()
    end)
  end)
end)
