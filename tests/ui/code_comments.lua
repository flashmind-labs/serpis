-- The code-comments extension (examples/extensions/code-comments.lua) installed as a user would
-- and used with real keys and clicks, against the local mock model (tests/ui/mock_llm.py): what
-- each message's request carries, and what the transcript shows.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
local out = os.getenv("SERPIS_TEST_OUT")
local project = out .. ".d"
sh("mkdir -p '" .. project .. "'")

local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("session-directories", {})
serpis.var.set("chat-compact-keep-tokens", 1) -- So that a few short messages can be compacted.

-- The text of the last request the mock got (the log is shared by every test: only the last
-- line is ours, right after a turn).
local log = os.getenv("SERPIS_TEST_MOCK_LOG")
local function lines()
  local list = {}
  local fh = io.open(log)
  if fh then
    for l in fh:lines() do table.insert(list, l) end
    fh:close()
  end
  return list
end
local function has(s, part) return s:find(part, 1, true) ~= nil end
-- Its log line, as JSON: {"n": .., "text": "..", ...}.
local function last_sent()
  local l = lines()
  return l[#l] or ""
end
-- Whether the last request's message was exactly `text`, with nothing before it.
local function sent_as_typed(text) return has(last_sent(), '"text": "' .. text .. '"') end
local function message_has(text)
  for _, m in ipairs(d.messages()) do
    if has(m, text) then return true end
  end
  return false
end
local function click(rect)
  local x, y = rect.x + rect.w / 2, rect.y + rect.h / 2
  d.mouse_move(x, y)
  d.mouse_button(x, y, "left", true)
  d.mouse_button(x, y, "left", false)
end
local function pill() return d.rect("header-button:code-comments-toggle") end

local RULE = "Never write any comments in the code."
local ALLOW = "You may write comments in code again"
local chat
local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  serpis.defer(200, go)
end
-- Send `text` and go on once the mock answered it and the chat is idle.
local function send(text)
  return function(go)
    local before = #lines()
    serpis.chat.prompt(chat, text)
    when(function() return #lines() > before and not serpis.chat.running(chat) and not serpis.chat.info(chat).compacting end, go)
    return false
  end
end

local i = 0
local list
local function go()
  i = i + 1
  local s = list[i]
  if not s then return T.finish() end
  local wait = s(go)
  if wait ~= false then serpis.defer(wait or 300, go) end
end

list = {
  function()
    local ext = serpis.config_dir .. "/extensions"
    sh("mkdir -p '" .. ext .. "' && cp '" .. os.getenv("SERPIS_TEST_DIR") .. "'/../../examples/extensions/code-comments.lua '" .. ext .. "'")
    serpis.reload()
    T.room()
    serpis.command.run("new-chat", project)
  end,
  function()
    chat = serpis.buffer.current()
    T.check("a new chat starts with comments off: the pill shows", pill() ~= nil, true)
  end,
  send("first"),
  function()
    local sent = last_sent()
    T.check("the first message carries the rule", has(sent, RULE), true)
    T.check("and the marker for comments it invalidates", has(sent, "TODO-UPDATE-COMMENT"), true)
    T.check("before the message itself", has(sent, '\\n\\nfirst"'), true)
    T.check("the transcript shows the message as typed", has(serpis.chat.transcript(chat), RULE), false)
    T.check("and the message", has(serpis.chat.transcript(chat), "first"), true)
  end,
  send("second"),
  function()
    T.check("the rule isn't said again", sent_as_typed("second"), true)
    T.press("C-c ;")
  end,
  function()
    T.check("C-c ; turns comments on", message_has("Code comments allowed in this chat"), true)
    T.check("and the pill goes", pill(), nil)
  end,
  send("third"),
  function()
    local sent = last_sent()
    T.check("the next message says comments are allowed again", has(sent, ALLOW), true)
    T.check("without the rule", has(sent, RULE), false)
  end,
  send("fourth"),
  function()
    T.check("and only once", sent_as_typed("fourth"), true)
    T.press("C-c ;")
  end,
  function()
    T.check("C-c ; again turns them off", pill() ~= nil, true)
    click(pill())
  end,
  function()
    T.check("a click on the pill turns them on", pill(), nil)
    T.press("C-c ;")
  end,
  function()
    T.check("off again", pill() ~= nil, true)
  end,
  send("fifth"),
  function()
    T.check("on, then off before a message: the rule again", has(last_sent(), RULE), true)
    serpis.command.run("chat-compact")
    when(function() return not serpis.chat.info(chat).compacting end, go)
    return false
  end,
  send("sixth"),
  function()
    T.check("after compacting, the rule comes with the next message", has(last_sent(), RULE), true)
    -- The choice lives in the session: resume it and it holds.
    local path = serpis.session.of(chat)
    T.check("session saved", path ~= nil, true)
    serpis.buffer.kill(chat)
    chat = serpis.session.resume(path)
    serpis.buffer.show(chat)
  end,
  function()
    T.check("resumed: still off", pill() ~= nil, true)
    T.check("its transcript shows messages as typed", has(serpis.chat.transcript(chat), RULE), false)
  end,
  send("seventh"),
  function()
    T.check("resumed: the agent was told already", sent_as_typed("seventh"), true)
    -- code-comments on: new chats start with comments allowed, and are told nothing.
    serpis.var.set("code-comments", true)
    serpis.command.run("new-chat", project)
  end,
  function()
    chat = serpis.buffer.current()
    T.check("code-comments on: a new chat has no pill", pill(), nil)
  end,
  send("eighth"),
  function()
    T.check("and its first message goes as typed", sent_as_typed("eighth"), true)
    T.press("C-c ;")
  end,
  send("ninth"),
  function()
    T.check("turned off, the next message carries the rule", has(last_sent(), RULE), true)
  end,
}
go()
