-- A first start with no keys anywhere opens the credentials page instead of a chat; a key
-- added there turns it into a way to start, and finishing it records so in state.json.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")

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

local function read(path)
  local f = io.open(path)
  if not f then return "" end
  local s = f:read("*a")
  f:close()
  return s
end

local function click(name)
  local r = d.rect(name)
  if not r then return false end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  d.mouse_button(x, y, "left", true)
  d.mouse_button(x, y, "left", false)
  return true
end

steps({
  function()
    T.check("a first start opens the credentials page", serpis.buffer.kind(), "welcome")
    T.check("instead of a chat", serpis.buffer.name(), "*Welcome*")
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
    T.check("the key is stored", read(agent_dir .. "/auth.json"):find("sk-or-test-123", 1, true) ~= nil, true)
    T.check("the page sees it", d.rect("chat-set-api-key"), nil)
    T.check("and offers to start", d.rect("setup-credentials-finish") ~= nil, true)
    T.check("clicking it works", click("setup-credentials-finish"), true)
  end,
  function()
    T.check("starting shows a chat", serpis.buffer.kind(), "chat")
    local state = read(serpis.config_dir .. "/state.json")
    T.check("the page is done with", state:find('"setup-done": true', 1, true) ~= nil, true)
    serpis.command.run("setup-credentials")
  end,
  function()
    T.check("setup-credentials brings it back", serpis.buffer.kind(), "welcome")
    T.check("with the key it already has", d.rect("chat-set-api-key"), nil)
    serpis.command.run("setup-credentials-finish")
  end,
  function()
    T.check("and finishing it again goes back to the chat", serpis.buffer.kind(), "chat")
    -- For welcome_restore, which runs next in this same home: only the config folder is left
    -- to tell a first start from a later one.
    local f = io.open(serpis.config_dir .. "/state.json", "w")
    f:write("{}")
    f:close()
  end,
})
