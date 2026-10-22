-- Compaction against serpis-mock-provider (a simulator of provider caches). The summary is asked
-- for in the conversation itself, so its request must read the prompt cache; the compacted chat
-- then misses once ("compacted"), resumes from its session with the same bytes, compacts on its
-- own near the context window, and recovers from a context-overflow error by compacting and
-- retrying the turn.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
local log = os.getenv("SERPIS_TEST_CACHE_LOG")
local function sh(cmd)
  local p = io.popen(cmd)
  local out = p:read("*a")
  p:close()
  return out
end
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"or":{"baseUrl":"%s/api/v1","api":"openai-completions","apiKey":"x","compat":{"openRouter":true},'
  .. '"models":[{"id":"anthropic/claude-sonnet-4.5","contextWindow":200000,"cost":{"input":3,"output":15,"cacheRead":0.3,"cacheWrite":3.75}}]}}}',
  os.getenv("SERPIS_TEST_CACHE_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("or/anthropic/claude-sonnet-4.5")
serpis.var.set("session-directories", {})
serpis.var.set("chat-compact-keep-tokens", 200)

local project = sh("mktemp -d"):gsub("%s+$", "")
for i = 0, 2 do
  local fh = io.open(project .. "/f" .. i .. ".txt", "w")
  fh:write(string.rep("contents of file " .. i .. "\n", 20))
  fh:close()
end

local function lines()
  local out = {}
  local fh = io.open(log)
  if fh then
    for l in fh:lines() do table.insert(out, l) end
    fh:close()
  end
  return out
end
local function field(line, name)
  return line and (line:match('"' .. name .. '":"([^"]*)"') or line:match('"' .. name .. '":(%d+)'))
end
local chat
local function has(text) return serpis.chat.transcript(chat):find(text, 1, true) ~= nil end

-- Wait until `ready()` (polled), then run `f`.
local function when(ready, f, tries)
  tries = tries or 0
  if not ready() and tries < 100 then return serpis.defer(100, function() when(ready, f, tries + 1) end) end
  serpis.defer(200, f)
end
local function idle() return not serpis.chat.running(chat) and serpis.chat.info(chat).compacting ~= true end
local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    local s = list[i]
    if not s then return T.finish() end
    local wait = s(go)
    if wait ~= false then serpis.defer(wait or 300, go) end
  end
  go()
end
-- Send `text` and continue once the mock has answered `n` more requests and the chat is idle.
local function turn(text, n)
  return function(go)
    local before = #lines()
    serpis.chat.prompt(chat, text)
    when(function() return #lines() >= before + n and idle() end, go)
    return false
  end
end

local mark, summary_line
steps({
  function()
    serpis.var.set("chat-compact-auto", false)
    serpis.command.run("new-chat", project)
    chat = serpis.buffer.current()
    serpis.chat.set_thinking(chat, "off")
  end,
  turn("hello", 1),
  turn("READ 3", 2),
  turn("next", 1),
  -- Manual compaction.
  function(go)
    mark = #lines()
    serpis.command.run("chat-compact")
    when(function() return has("Compacted ") end, go)
    return false
  end,
  function()
    local new = lines()
    summary_line = new[mark + 1]
    T.check("one request asks for the summary", #new, mark + 1)
    T.check("it reads the prompt cache", field(summary_line, "miss"), "none")
    T.check("everything up to the cut", tonumber(field(summary_line, "read") or 0) > 1000, true)
    T.check("only the instruction is new", tonumber(field(summary_line, "write")) + tonumber(field(summary_line, "input")) < 600, true)
    T.check("the chat says what it did", has("Compacted ") and has("tokens from the prompt cache"), true)
    local path = serpis.session.of(chat)
    T.check("the session records a pi compaction entry", sh("grep -c '\"type\":\"compaction\"' '" .. path .. "'"):gsub("%s+", ""), "1")
  end,
  turn("after compaction", 1),
  function()
    local misses = serpis.chat.cache(chat).misses
    T.check("the chat names the next miss: compacted", (misses[#misses] or {}).cause, "compacted")
  end,
  turn("again", 1),
  function()
    local l = lines()
    T.check("then the compacted conversation hits", field(l[#l], "miss"), "none")
    -- Resumed from its session, the compacted chat sends the same bytes.
    local path = serpis.session.of(chat)
    serpis.buffer.kill(chat)
    chat = serpis.session.resume(path)
  end,
  turn("resumed", 1),
  function()
    local l = lines()
    T.check("a resumed compacted chat reads the cache", field(l[#l], "miss"), "none")
    -- Automatic: a context of more than 1,000 tokens is over the threshold here.
    serpis.var.set("chat-compact-auto", true)
    serpis.var.set("chat-compact-reserve-tokens", 199000)
    serpis.command.run("new-chat", project)
    chat = serpis.buffer.current()
    serpis.chat.set_thinking(chat, "off")
  end,
  turn("hello", 1),
  turn("READ 3", 2),
  function(go)
    when(function() return has("Compacted ") end, go)
    return false
  end,
  function()
    T.check("near the window the chat compacts on its own", has("Compacted "), true)
    -- Overflow: the provider refuses the prompt; the chat compacts and retries the turn.
    serpis.var.set("chat-compact-reserve-tokens", 16384)
    serpis.command.run("new-chat", project)
    chat = serpis.buffer.current()
    serpis.chat.set_thinking(chat, "off")
  end,
  turn("hello", 1),
  turn("READ 3", 2),
  function(go)
    mark = #lines()
    serpis.chat.prompt(chat, "OVERFLOW please")
    -- The refused request, the summary, the retried turn.
    when(function() return #lines() >= mark + 3 and idle() end, go)
    return false
  end,
  function()
    local l = lines()
    T.check("an overflow compacts the chat", has("Compacted "), true)
    T.check("and the turn is retried", #l, mark + 3)
    T.check("successfully", serpis.chat.transcript(chat):match("Compacted .*\nok") ~= nil, true)
  end,
})
