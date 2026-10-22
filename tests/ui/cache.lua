-- Prompt caching in the real app, against serpis-mock-provider: a simulator of provider caches
-- that logs each request's verdict (read, write, miss cause). The chat runs through an
-- OpenRouter-style provider. Editing CLAUDE.md mid-chat must not miss (the system prompt is kept),
-- chat-refresh-context misses once on the system prompt, a thinking change is the stated cause,
-- and a resumed chat keeps its session id and reads the cache it left. The chat's own view of its
-- cache (from what the provider reports) must agree: the chip on each response, the header entry,
-- miss notices with their cause, and warnings before actions that throw the cache away.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
local chat
local veto = false
local status_x, warned
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
  .. '"models":[{"id":"anthropic/claude-sonnet-4.5","reasoning":true,"cost":{"input":3,"output":15,"cacheRead":0.3,"cacheWrite":3.75}}]}}}',
  os.getenv("SERPIS_TEST_CACHE_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("or/anthropic/claude-sonnet-4.5")
serpis.var.set("session-directories", {})
-- Test prompts are small: warn about any cost.
serpis.var.set("cache-warn-threshold", 0)
local d = serpis.debug
local function last_message(prefix)
  local ms = d.messages()
  for i = #ms, 1, -1 do
    if not prefix or ms[i]:find(prefix, 1, true) then return ms[i] end
  end
end
local function count_messages(text)
  local n = 0
  for _, m in ipairs(d.messages()) do if m:find(text, 1, true) then n = n + 1 end end
  return n
end
local function transcript_has(text) return serpis.chat.transcript(chat):find(text, 1, true) ~= nil end

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

-- Who served each response, from the assistant-message hook.
local upstreams = {}
serpis.hook.add("assistant-message", function(p) table.insert(upstreams, p.upstream or "?") end)

steps({
  function()
    serpis.command.run("new-chat", project)
    chat = serpis.buffer.current()
    serpis.chat.set_thinking(chat, "off")
  end,
  turn("hello", 1, "none", "the first request has nothing to miss"),
  function()
    T.check("the first response's chip says the cache is new", transcript_has("cache new"), true)
    T.check("the chat knows its cache is warm", serpis.chat.cache(chat).state, "warm")
  end,
  turn("READ 3", 2, "none,none", "three parallel reads and the answer read the cache"),
  function()
    T.check("a later response's chip shows its hit share", serpis.chat.transcript(chat):match("cache %d+%%") ~= nil, true)
    T.check("the hit rate counts reads", serpis.chat.cache(chat).hit_rate > 0, true)
    if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/cache-chip.png") end
    -- The header's cache entry opens the report.
    local r = d.rect("header-status-button")
    T.check("the header shows a clickable cache entry", r ~= nil, true)
    if r then
      d.mouse_move(r.x + r.w / 2, r.y + r.h / 2)
      serpis.defer(100, function()
        d.mouse_button(r.x + r.w / 2, r.y + r.h / 2, "left", true)
        d.mouse_button(r.x + r.w / 2, r.y + r.h / 2, "left", false)
      end)
    end
  end,
  function()
    local cur = serpis.buffer.current()
    T.check("clicking it shows the cache report", serpis.buffer.name(cur), "*cache*")
    T.check("which says the cache is warm", serpis.buffer.text(cur):find("Warm: it expires in", 1, true) ~= nil, true)
    serpis.buffer.show(chat)
  end,
  function()
    write("CLAUDE.md", "Use tabs.\nAnd write tests.\n")
  end,
  turn("after the edit", 1, "none", "editing CLAUDE.md mid-chat keeps the cache"),
  function()
    serpis.command.run("chat-refresh-context")
    T.check("refreshing warns what it costs", (last_message("Refreshing the context re-caches") or ""):match("[~<]%$%d") ~= nil, true)
  end,
  turn("refreshed", 1, "prefix:system[0]", "chat-refresh-context rewrites the cache once, from the system prompt"),
  function()
    T.check("the chat names the miss's cause inline", transcript_has("cache miss: the system prompt was refreshed"), true)
    T.check("and in serpis.chat.cache", (serpis.chat.cache(chat).misses[1] or {}).cause, "prompt")
  end,
  turn("again", 1, "none", "and it holds after that"),
  function()
    -- confirm: asked first; answering no keeps the level and the cache.
    serpis.var.set("cache-warn", "confirm")
    serpis.command.run("chat-cycle-thinking")
  end,
  function()
    T.check("confirm asks before a thinking change", (d.minibuffer() or ""):find("Changing the thinking level re-caches", 1, true) ~= nil, true)
    d.text("no")
  end,
  function() d.key("Enter") end,
  function()
    T.check("no keeps the thinking level", serpis.chat.info(chat).thinking, "off")
    serpis.var.set("cache-warn", "notice")
    -- A hook can keep the cache.
    serpis.hook.add("before-cache-invalidate", function(p) if p.action == "thinking" and veto then return false end end)
    veto = true
    serpis.command.run("chat-select-thinking", "low")
  end,
  function()
    T.check("a before-cache-invalidate hook can veto", serpis.chat.info(chat).thinking, "off")
    T.check("and says so", last_message("Kept the prompt cache") ~= nil, true)
    veto = false
    serpis.command.run("chat-select-thinking", "high")
  end,
  function()
    T.check("notice: the change happens", serpis.chat.info(chat).thinking, "high")
    T.check("and the warning says what it costs", last_message("Changing the thinking level re-caches") ~= nil, true)
  end,
  turn("think", 1, "params", "a thinking change is the stated cause"),
  function()
    T.check("the chat agrees on the cause", (serpis.chat.cache(chat).misses[2] or {}).cause, "thinking")
    if os.getenv("SERPIS_TEST_SHOTS") then d.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/cache-miss.png") end
    -- An hour passes (the default cache lives 1 hour): on the provider and in serpis.
    sh("curl -s '" .. os.getenv("SERPIS_TEST_CACHE_URL") .. "/control/advance/3700'")
    d.cache_age(chat, 3700)
    serpis.chat.info(chat)
  end,
  function()
    T.check("an hour later the cache is cold", serpis.chat.cache(chat).state, "cold")
  end,
  turn("back", 1, "expired", "after an hour the provider's cache expired"),
  function()
    T.check("sending warned that it had expired", last_message("The prompt cache expired") ~= nil, true)
    T.check("and the miss is named as expiry", (serpis.chat.cache(chat).misses[3] or {}).cause, "expired")
  end,
  function()
    local path = serpis.session.of(chat)
    T.check("the chat was saved", path ~= nil, true)
    serpis.buffer.kill(chat)
    chat = serpis.session.resume(path)
  end,
  turn("resumed", 1, "none", "a resumed chat reads the cache it left"),
  -- Every part of the cache UI can be turned off.
  function()
    serpis.window.only()
    serpis.buffer.show(chat)
  end,
  function()
    local r = d.rect("header-status-button")
    status_x = r and r.x
    T.check("the header's cache entry is back after the resume", serpis.chat.cache(chat).state, "warm")
    serpis.var.set("chat-cache-status", false)
    serpis.var.set("chat-cache-indicator", false)
    serpis.var.set("chat-cache-miss-notice", "off")
    serpis.var.set("cache-warn", "off")
  end,
  function()
    local r = d.rect("header-status-button")
    T.check("chat-cache-status off: the leftmost header entry is the model again", status_x ~= nil and r ~= nil and r.x > status_x, true)
    T.check("chat-cache-indicator off: no chips", serpis.chat.transcript(chat):match("cache %d+%%") == nil and not transcript_has("cache new"), true)
    warned = count_messages("Changing the thinking level re-caches")
    serpis.command.run("chat-select-thinking", "low")
  end,
  function()
    T.check("cache-warn off: the change happens", serpis.chat.info(chat).thinking, "low")
    T.check("without a warning", count_messages("Changing the thinking level re-caches"), warned)
  end,
  turn("quiet", 1, "params", "the provider still misses on a thinking change"),
  function()
    T.check("chat-cache-miss-notice off: no notice", transcript_has("cache miss:"), false)
    T.check("the miss is still recorded", (serpis.chat.cache(chat).misses[1] or {}).cause, "thinking")
  end,
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
    T.check("responses name their OpenRouter upstream", #upstreams > 0 and upstreams[1]:match("^upstream%-[ab]$") ~= nil, true)
  end,
})
