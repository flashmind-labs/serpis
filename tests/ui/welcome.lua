-- A start with no keys anywhere opens the credentials page instead of a chat; adding a key
-- there closes it onto a chat.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then return T.finish() end
    list[i]()
    serpis.defer(400, go)
  end
  go()
end

steps({
  function()
    T.check("no keys opens the credentials page", serpis.buffer.kind(), "welcome")
    T.check("it offers a key", d.rect("chat-set-api-key") ~= nil, true)
    serpis.command.run("chat-set-api-key")
  end,
  function()
    T.check("chat-set-api-key asks which provider", d.minibuffer(), "Provider: ")
    d.text("openrouter")
    d.key("Enter")
  end,
  function()
    T.check("then for the key", d.minibuffer(), "API key for openrouter: ")
    d.text("sk-or-test-123")
    d.key("Enter")
  end,
  function()
    T.check("a key closes the page onto a chat", serpis.buffer.kind(), "chat")
  end,
})
