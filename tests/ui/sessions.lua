-- Chats record sessions and resume them. tests/ui/sessions_restore.lua runs next with the same
-- config directory and checks the chat left open here comes back.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local shots = os.getenv("SERPIS_TEST_SHOTS")
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
sh("rm -rf '" .. tmp .. "' && mkdir -p '" .. tmp .. "/proj' && cd '" .. tmp .. "/proj' && git init -q -b main")
local proj = sh("cd '" .. tmp .. "/proj' && pwd -P")
-- A provider that refuses connections: runs fail fast, but messages are still recorded.
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write('{"providers":{"fake":{"baseUrl":"http://127.0.0.1:9/v1","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}')
f:close()
serpis.agent.reload_models()
serpis.agent.model("fake/m")
serpis.var.set("session-directories", {})

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    local s = list[i]
    if not s then return T.finish() end
    -- A step returning false calls `go` itself.
    local wait = s(go)
    if wait ~= false then serpis.defer(wait or 300, go) end
  end
  go()
end
local function rows(kind)
  local out = {}
  for _, r in ipairs(d.sidebar_rows()) do
    if r.kind == kind then table.insert(out, r) end
  end
  return out
end
-- Run `f` once the chat has stopped working (the fake provider fails after its retries).
local function settled(id, f, n)
  n = n or 0
  if serpis.buffer.activity(id).state == "working" and n < 100 then
    return serpis.defer(200, function() settled(id, f, n + 1) end)
  end
  f()
end
local function count(path)
  return tonumber(sh("grep -c '\"type\":\"message\"' '" .. path .. "'"))
end

local chat, path
steps({
  function()
    T.check("sessions dir", serpis.session.directory(), agent_dir .. "/sessions")
    serpis.command.run("new-chat", proj)
    chat = serpis.buffer.current()
    T.check("no session before a prompt", serpis.session.of(chat), nil)
    d.text("hello there")
    T.press("RET")
    return 500
  end,
  function(next)
    settled(chat, next)
    return false
  end,
  function()
    T.check("run failed fast", serpis.buffer.activity(chat).state ~= "working", true)
    T.check("project pinned", serpis.var.get("projects")[1], proj)
    path = serpis.session.of(chat)
    T.check("session recorded", path ~= nil, true)
    T.check("in pi's layout", path and path:find("/sessions/%-%-.*proj%-%-/") ~= nil, true)
    T.check("prompt and reply saved", path and count(path), 2)
    T.check("header first", path and sh("head -c 17 '" .. path .. "'"), '{"type":"session"')
    T.check("titled by prompt", serpis.buffer.title(chat), "hello there")
    serpis.buffer.set_title(chat, "Greeting")
    serpis.buffer.kill(chat)
    serpis.command.run("session-rescan")
    return 800
  end,
  function()
    local found = false
    for _, r in ipairs(rows("session")) do
      if r.path == path then found = true end
    end
    T.check("sidebar lists closed session", found, true)
    if shots then d.screenshot(shots .. "/sessions-sidebar.png") end
    -- /resume with real keys.
    T.press("s-r")
    return 600
  end,
  function()
    T.press("RET")
    return 400
  end,
  function()
    chat = serpis.buffer.current()
    T.check("s-r RET resumes", serpis.session.of(chat), path)
    T.check("named title restored", serpis.buffer.title(chat), "Greeting")
    T.check("transcript restored", (serpis.buffer.text() or ""):find("hello there") ~= nil, true)
    if shots then d.screenshot(shots .. "/sessions-resumed.png") end
    -- Continuing appends to the same file.
    d.text("again")
    T.press("RET")
    return 500
  end,
  function(next)
    settled(chat, next)
    return false
  end,
  function()
    T.check("continued in same file", count(path), 4)
    T.check("same chat", serpis.session.of(serpis.buffer.current()), path)
    -- Clicking a closed session in the sidebar resumes it.
    serpis.buffer.kill(chat)
    serpis.command.run("session-rescan")
    return 800
  end,
  function()
    for _, r in ipairs(rows("session")) do
      if r.path == path then
        d.mouse_move(r.x + r.w / 3, r.y + r.h / 2)
        d.mouse_button(r.x + r.w / 3, r.y + r.h / 2, "left", true)
        d.mouse_button(r.x + r.w / 3, r.y + r.h / 2, "left", false)
      end
    end
    return 500
  end,
  function()
    chat = serpis.buffer.current()
    T.check("click resumes", serpis.session.of(chat), path)
    T.check("resumed has 4 messages", (serpis.buffer.text() or ""):find("again") ~= nil, true)
    -- Sidebar search finds closed sessions by their prompts.
    serpis.buffer.kill(chat)
    serpis.command.run("session-rescan")
    return 800
  end,
  function()
    -- Only what the search is about: the chat serpis started with (unseen now, its folder's
    -- path matching too) goes first.
    for _, b in ipairs(serpis.buffer.list()) do
      if b.kind == "chat" and serpis.session.of(b.id) == nil then serpis.buffer.kill(b.id) end
    end
    T.press("s-e")
    T.press("a g a i n")
    return 500
  end,
  function()
    local sel = serpis.sidebar.selected()
    T.check("search selects session", sel and sel.kind, "session")
    T.check("search finds it", sel and sel.path, path)
    T.press("RET")
    return 500
  end,
  function()
    T.check("RET resumes from search", serpis.session.of(serpis.buffer.current()), path)
    -- Left open: sessions_restore.lua expects it back.
    local f = io.open(os.getenv("SERPIS_TEST_OUT") .. ".path", "w")
    f:write(path)
    f:close()
  end,
})
