-- Prompt caching in the real app, against serpis-mock-provider: a simulator of provider caches
-- that logs each request's verdict (read, write, miss cause). The chat runs through an
-- OpenRouter-style provider. Editing CLAUDE.md mid-chat must not miss (the system prompt is kept),
-- chat-refresh-context misses once on the system prompt, a thinking change is the stated cause,
-- and a resumed chat keeps its session id and reads the cache it left.
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
  .. '"models":[{"id":"anthropic/claude-sonnet-4.5","reasoning":true}]}}}', os.getenv("SERPIS_TEST_CACHE_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("or/anthropic/claude-sonnet-4.5")
serpis.var.set("session-directories", {})

local project = sh("mktemp -d"):gsub("%s+$", "")
local function write(path, text)
  local fh = io.open(project .. "/" .. path, "w")
  fh:write(text)
  fh:close()
end
write("CLAUDE.md", "Use tabs.\n")
for i = 0, 2 do write("f" .. i .. ".txt", string.rep("contents of file " .. i .. "\n", 20)) end

local function lines()
  local out = {}
  local fh = io.open(log)
  if fh then
    for l in fh:lines() do table.insert(out, l) end
    fh:close()
  end
  return out
end
local base = #lines()
local function requests()
  local all, out = lines(), {}
  for i = base + 1, #all do table.insert(out, all[i]) end
  return out
end
local function field(line, name)
  return line:match('"' .. name .. '":"([^"]*)"') or line:match('"' .. name .. '":(%d+)')
end
-- The misses of requests after the first `from`, joined: "none,none" or "prefix:system[0]".
local function misses(from)
  local r, out = requests(), {}
  for i = from + 1, #r do table.insert(out, field(r[i], "miss")) end
  return table.concat(out, ",")
end

local chat
local function settled(f, n, tries)
  tries = tries or 0
  if (serpis.chat.running(chat) or #requests() < n) and tries < 100 then
    return serpis.defer(100, function() settled(f, n, tries + 1) end)
  end
  serpis.defer(200, f)
end
-- Send `text`, wait for `n` more requests, then check their misses.
local seen = 0
local function turn(text, n, want, label)
  return function(go)
    serpis.chat.prompt(chat, text)
    settled(function()
      T.check(label, misses(seen), want)
      seen = #requests()
      go()
    end, seen + n)
    return false
  end
end

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

steps({
  function()
    serpis.command.run("new-chat", project)
    chat = serpis.buffer.current()
    serpis.chat.set_thinking(chat, "off")
  end,
  turn("hello", 1, "none", "the first request has nothing to miss"),
  turn("READ 3", 2, "none,none", "three parallel reads and the answer read the cache"),
  function()
    write("CLAUDE.md", "Use tabs.\nAnd write tests.\n")
  end,
  turn("after the edit", 1, "none", "editing CLAUDE.md mid-chat keeps the cache"),
  function()
    serpis.command.run("chat-refresh-context")
  end,
  turn("refreshed", 1, "prefix:system[0]", "chat-refresh-context rewrites the cache once, from the system prompt"),
  turn("again", 1, "none", "and it holds after that"),
  function()
    serpis.chat.set_thinking(chat, "high")
  end,
  turn("think", 1, "params", "a thinking change is the stated cause"),
  function()
    local path = serpis.session.of(chat)
    T.check("the chat was saved", path ~= nil, true)
    serpis.buffer.kill(chat)
    chat = serpis.session.resume(path)
  end,
  turn("resumed", 1, "none", "a resumed chat reads the cache it left"),
  function()
    local r = requests()
    local ids, long = {}, true
    for _, l in ipairs(r) do
      ids[field(l, "session") or "?"] = true
      if tonumber(field(l, "write")) > 0 and field(l, "write") ~= field(l, "write_1h") then long = false end
    end
    local n = 0
    for _ in pairs(ids) do n = n + 1 end
    T.check("every request, before and after the resume, has one session id", n, 1)
    T.check("cache writes are 1-hour by default (agent-cache-retention)", long, true)
    T.check("no request was rejected", sh("grep -c rejected '" .. log .. "'"):gsub("%s+", ""), "0")
  end,
})
