-- Runs after cache.lua with the same config and mock provider: serpis restarts, and the chat
-- cache.lua left open is restored. CLAUDE.md changed while serpis was closed, but the chat's
-- prompt cache is still warm, so it keeps the system prompt it was sent with and says so; the next
-- message reads the cache. chat-refresh-context then picks up CLAUDE.md, rewriting it once.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local log = os.getenv("SERPIS_TEST_CACHE_LOG")
local out = os.getenv("SERPIS_TEST_OUT"):gsub("cache_restore%.out$", "cache.out")
local f = io.open(out .. ".path")
local path = f and f:read("*a")
if f then f:close() end

local chat
for _, b in ipairs(serpis.buffer.list()) do
  if path and serpis.session.of(b.id) == path then chat = b.id end
end
T.check("the chat left open is restored", chat ~= nil, true)
if not chat then return T.finish() end

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
  return line:match('"' .. name .. '":"([^"]*)"') or line:match('"' .. name .. '":(%d+)')
end
local before = lines()
local session = field(before[#before], "session")
local function turn(text, want, label, after)
  serpis.chat.prompt(chat, text)
  local tries = 0
  local function wait()
    tries = tries + 1
    if (serpis.chat.running(chat) or #lines() <= #before) and tries < 100 then return serpis.defer(100, wait) end
    local all = lines()
    local last = all[#all]
    T.check(label, field(last, "miss"), want)
    T.check(label .. " (same session id)", field(last, "session"), session)
    before = all
    serpis.defer(200, after)
  end
  serpis.defer(200, wait)
end

serpis.buffer.show(chat)
-- The restored chat compares tools and instructions once serpis has finished starting.
serpis.defer(500, function()
  T.check("it says it kept the system prompt", serpis.chat.transcript(chat):find("Kept the system prompt this chat was sent with", 1, true) ~= nil, true)
  T.check("its cache is still warm", serpis.chat.cache(chat).state, "warm")
  turn("after the restart", "none", "the first message after the restart reads the cache", function()
    serpis.var.set("cache-warn", "off")
    serpis.command.run("chat-refresh-context")
    turn("refreshed", "prefix:system[0]", "chat-refresh-context then picks up CLAUDE.md", function()
      T.check("and the chat names the cause", (serpis.chat.cache(chat).misses[1] or {}).cause, "prompt")
      T.finish()
    end)
  end)
end)
