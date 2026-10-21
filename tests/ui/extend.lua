-- Serpis extended from within: the example extensions (ports of pi's max-money and
-- model-presets) installed as a user would, chats driven end to end against the local mock
-- model (tests/ui/mock_llm.py; every response costs $1.00 here), and the agent tools an agent
-- uses to write extensions (serpis_lua, serpis_api) called the way a model calls them.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local function sh(cmd)
  local p = io.popen(cmd .. " 2>&1")
  local out = p:read("*a")
  p:close()
  return (out:gsub("%s+$", ""))
end
local tmp = os.getenv("SERPIS_TEST_OUT") .. ".d"
sh("rm -rf '" .. tmp .. "' && mkdir -p '" .. tmp .. "'")
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
local log = os.getenv("SERPIS_TEST_MOCK_LOG")
sh("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":['
  .. '{"id":"m","cost":{"input":1000,"output":0}},{"id":"m2","reasoning":true,"cost":{"input":1000,"output":0}}]}}}',
  os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("session-directories", {})

-- The mock's request log, from this test on.
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
local function last_message(prefix)
  local ms = d.messages()
  for i = #ms, 1, -1 do
    if not prefix or ms[i]:sub(1, #prefix) == prefix then return ms[i] end
  end
end
local function commands()
  local set = {}
  for _, c in ipairs(serpis.command.list()) do set[c] = true end
  return set
end
local function cost(buf)
  return string.format("%.2f", serpis.chat.usage(buf).cost)
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
-- Run `f` once the chat's agent has stopped (and the mock has seen `n` requests, if given).
local function settled(buf, f, n, tries)
  tries = tries or 0
  local busy = serpis.chat.running(buf) or (n and #requests() < n)
  if busy and tries < 100 then
    return serpis.defer(100, function() settled(buf, f, n, tries + 1) end)
  end
  serpis.defer(300, f)
end

local chat, other
steps({
  function()
    -- Install the extensions as a user (or an agent) would: drop the files in extensions/.
    local ext = serpis.config_dir .. "/extensions"
    sh("mkdir -p '" .. ext .. "' && cp '" .. os.getenv("SERPIS_TEST_DIR") .. "'/../../examples/extensions/*.lua '" .. ext .. "'")
    serpis.reload()
    local cmds = commands()
    T.check("max-money loaded", cmds["max-money"] ~= nil, true)
    T.check("model presets loaded", cmds.heavy ~= nil and cmds.light ~= nil and cmds.extra ~= nil, true)
    T.check("extensions load without errors", last_message("error"), nil)
    serpis.command.run("new-chat", tmp)
    chat = serpis.buffer.current()
    T.check("a chat", serpis.buffer.kind(chat), "chat")
  end,
  function()
    -- The default presets name OpenRouter models, which this registry can't use.
    serpis.command.run("heavy")
    local m = last_message("error") or ""
    T.check("preset without that provider fails", m:find("openrouter", 1, true) ~= nil, true)
    T.check("and leaves the model alone", serpis.chat.info(chat).model, "mock/m")
    -- Presets are a setting: replace them and reload.
    serpis.var.set("model-presets", { fast = { model = "mock/m2", label = "M2 fast", thinking = "high" } })
    serpis.reload()
    local cmds = commands()
    T.check("custom preset defined", cmds.fast ~= nil, true)
    T.check("default presets gone", cmds.heavy, nil)
    serpis.command.run("fast")
  end,
  function()
    local info = serpis.chat.info(chat)
    T.check("preset switches the chat's model", info.model, "mock/m2")
    T.check("and thinking", info.thinking, "high")
    T.check("preset message", last_message(), "model: M2 fast")
    T.check("default model untouched", serpis.var.get("agent-model"), "mock/m")
    local ok, err = pcall(serpis.chat.set_model, chat, "mock/nope")
    T.check("unknown model raises", not ok and tostring(err):find("not found", 1, true) ~= nil, true)
    serpis.chat.set_model(chat, "mock/m")
    serpis.command.run("max-money", 1.5)
    T.check("limit recorded in the session", serpis.chat.entries(chat, "max-money")[1].limit, 1.5)
    T.check("limit message", last_message(), "Will stop once the session spends more than $1.50")
    -- The presets command: a list to pick from.
    serpis.var.set("model-presets", {
      fast = { model = "mock/m2", label = "M2 fast", thinking = "high" },
      plain = { model = "mock/m", label = "M plain", thinking = "off" },
    })
    serpis.reload()
    serpis.chat.set_thinking(chat, "off")
  end,
  function()
    serpis.command.run("presets")
  end,
  function()
    local c = d.candidates()
    T.check("presets lists them in order", #c == 2 and c[1].label .. "|" .. c[2].label, "fast|plain")
    T.check("with what they switch to", c[1].detail, "M2 fast")
    T.check("marking the current one", c[2].detail, "M plain  ✓")
    d.key("Enter")
  end,
  function()
    T.check("picking one switches the chat", serpis.chat.info(chat).model, "mock/m2")
    T.check("the picker closes", d.minibuffer(), nil)
    serpis.chat.set_model(chat, "mock/m")
    serpis.chat.set_thinking(chat, "off")
  end,
  function(go)
    T.check("back on mock/m", serpis.chat.info(chat).model, "mock/m")
    serpis.chat.prompt(chat, "hello")
    settled(chat, go, 1)
    return false
  end,
  function(go)
    T.check("a response costs $1.00", cost(chat), "1.00")
    local ctx = serpis.chat.info(chat).context
    T.check("context used is the last response's tokens", ctx.used, 1010)
    T.check("of the model's window", ctx.window > 0 and string.format("%.4f", ctx.percent) == string.format("%.4f", 1010 * 100 / ctx.window), true)
    if os.getenv("SERPIS_TEST_SHOTS") then serpis.debug.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/usage.png") end
    serpis.chat.prompt(chat, "again")
    settled(chat, go, 2)
    return false
  end,
  function()
    T.check("over the limit after the second", cost(chat), "2.00")
    T.check("two requests", #requests(), 2)
    -- Typed into the composer: refused, and the text stays to edit or send later.
    d.text("third")
  end,
  function()
    d.key("Enter")
  end,
  function()
    T.check("refused prompt stays in the composer", d.composer_state().text, "third")
    T.check("no request for it", #requests(), 2)
    T.check("refusal says why", last_message("warning"), "warning: Spent $2.00, over the $1.50 limit. Set a higher one with max-money to carry on.")
    -- A higher limit lets it carry on; a response asking for a tool then crosses it: the run
    -- is aborted and the tool blocked, so no further request is made.
    serpis.command.run("max-money", 2.5)
    serpis.chat.prompt(chat, "TOOL please")
  end,
  function(go)
    settled(chat, go, 3)
    return false
  end,
  function()
    T.check("crossing the limit costs that response", cost(chat), "3.00")
    T.check("no request after the abort", #requests(), 3)
    T.check("stopped message", last_message("error"), "error: Spent $3.00, over the $2.50 limit. Set a higher one with max-money to carry on.")
    T.check("agent stopped", serpis.chat.running(chat), false)
    -- The limit lives in the session: resume it and it still holds.
    local path = serpis.session.of(chat)
    T.check("session saved", path ~= nil, true)
    serpis.buffer.kill(chat)
    chat = serpis.session.resume(path)
  end,
  function()
    T.check("limit restored on resume", serpis.chat.entries(chat, "max-money")[2].limit, 2.5)
    T.check("spend restored on resume", cost(chat), "3.00")
    serpis.chat.prompt(chat, "still over?")
  end,
  function()
    T.check("resumed chat still refuses", #requests(), 3)
    -- The agent tools, in a chat without a limit.
    serpis.command.run("new-chat", tmp)
    other = serpis.buffer.current()
    serpis.chat.prompt(other, "LUA:print('hi', 1 + 1); return serpis.chat.info(CHAT).buffer")
  end,
  function(go)
    settled(other, go, 5)
    return false
  end,
  function()
    local r = requests()
    T.check("agents get the serpis tools", r[4]:find('"serpis_lua"', 1, true) ~= nil and r[4]:find('"serpis_api"', 1, true) ~= nil, true)
    T.check("serpis_lua: CHAT is the calling chat", r[5]:find(tostring(other), 1, true) ~= nil, true)
    T.check("serpis_lua: print output", r[5]:find("printed:\\nhi\\t2", 1, true) ~= nil, true)
    serpis.chat.prompt(other, "LUA:error('boom')")
  end,
  function(go)
    settled(other, go, 7)
    return false
  end,
  function()
    local r = requests()
    T.check("serpis_lua: errors are reported", r[7]:find("error: ", 1, true) ~= nil and r[7]:find("boom", 1, true) ~= nil, true)
    serpis.chat.prompt(other, "LUA:while true do end")
  end,
  function(go)
    settled(other, go, 9)
    return false
  end,
  function()
    local r = requests()
    T.check("serpis_lua: runaway code is stopped", r[9] and r[9]:find("stopped after 5 s", 1, true) ~= nil, true)
    serpis.chat.prompt(other, "API:guide")
  end,
  function(go)
    settled(other, go, 11)
    return false
  end,
  function()
    local r = requests()
    T.check("serpis_api: guide names the extensions dir", r[11]:find("/extensions/NAME.lua", 1, true) ~= nil, true)
    T.check("serpis_api: guide lists loaded extensions", r[11]:find("Loaded now: max-money, model-presets", 1, true) ~= nil, true)
    serpis.chat.prompt(other, "API:hooks")
  end,
  function(go)
    settled(other, go, 13)
    return false
  end,
  function()
    local r = requests()
    T.check("serpis_api: hooks", r[13]:find("before-prompt: {buffer, text", 1, true) ~= nil, true)
    -- An extension the agent wrote, loaded and used without leaving the chat.
    local code = "local f = io.open(serpis.config_dir .. '/extensions/hello.lua', 'w') "
      .. "f:write('serpis.command.define(\"hello-ext\", function() serpis.message(\"hello from an extension\") end, \"Say hi.\")') "
      .. "f:close() serpis.reload() serpis.command.run('hello-ext')"
    serpis.chat.prompt(other, "LUA:" .. code)
  end,
  function(go)
    settled(other, go, 15)
    return false
  end,
  function()
    local r = requests()
    T.check("a new extension works at once", r[15]:find("hello from an extension", 1, true) ~= nil, true)
    T.check("and is a command", commands()["hello-ext"], true)
    -- Thinking can be hidden (to its header) and shown again; C-t as in the user's config.
    serpis.keymap.set("chat", "C-t", "chat-toggle-thinking")
    serpis.chat.prompt(other, "THINK hard")
  end,
  function(go)
    settled(other, go, 16)
    return false
  end,
  function()
    local t = serpis.chat.transcript(other)
    T.check("thinking shown by default", t:find("step 6", 1, true) ~= nil, true)
    d.key("T", { "ctrl" })
  end,
  function()
    local t = serpis.chat.transcript(other)
    T.check("C-t hides thinking", serpis.var.get("chat-show-thinking"), false)
    T.check("hidden thinking keeps its header", t:find("Thought · 6 lines", 1, true) ~= nil, true)
    T.check("but not its text", t:find("step 6", 1, true), nil)
    d.key("T", { "ctrl" })
  end,
  function()
    T.check("C-t shows it again", serpis.chat.transcript(other):find("step 6", 1, true) ~= nil, true)
    -- TAB on the thinking unfolds it in full, then folds it to its header, like tool output.
    serpis.command.run("chat-focus-transcript")
    local n = 0
    for l in (serpis.chat.transcript(other) .. "\n"):gmatch("(.-)\n") do
      n = n + 1
      if l:find("Thought", 1, true) then break end
    end
    serpis.buffer.set_cursor(n, 1)
    d.key("Tab")
  end,
  function()
    local t = serpis.chat.transcript(other)
    T.check("TAB shows all of the thinking", t:find("step 1", 1, true) ~= nil and t:find("step 6", 1, true) ~= nil, true)
    d.key("Tab")
  end,
  function()
    local t = serpis.chat.transcript(other)
    T.check("TAB again folds it to its header", t:find("step 6", 1, true) == nil and t:find("Thought", 1, true) ~= nil, true)
    d.key("T", { "ctrl" })
  end,
  function()
    d.key("T", { "ctrl" })
  end,
  function()
    local t = serpis.chat.transcript(other)
    T.check("C-t twice resets folds by hand", t:find("step 6", 1, true) ~= nil and t:find("step 1", 1, true) == nil, true)
    -- Code in answers and in read output (highlighting itself is unit-tested in highlight.rs).
    local f = io.open(tmp .. "/lib.rs", "w")
    f:write("/// Doubles it.\npub fn double(x: u32) -> u32 {\n    x * 2\n}\n")
    f:close()
    serpis.chat.prompt(other, "CODE please")
  end,
  function(go)
    settled(other, go, 17)
    return false
  end,
  function()
    T.check("code blocks render", serpis.chat.transcript(other):find("fn add(a: i32, b: i32) -> i32 {", 1, true) ~= nil, true)
    serpis.chat.prompt(other, "READ:" .. tmp .. "/lib.rs")
  end,
  function(go)
    settled(other, go, 19)
    return false
  end,
  function()
    -- Read output starts folded: TAB on its card opens it.
    serpis.command.run("chat-focus-transcript")
    local n, at = 0, nil
    for l in (serpis.chat.transcript(other) .. "\n"):gmatch("(.-)\n") do
      n = n + 1
      if l:find("lib.rs", 1, true) then at = n end
    end
    serpis.buffer.set_cursor(at or 1, 1)
    d.key("Tab")
  end,
  function()
    T.check("read output renders", serpis.chat.transcript(other):find("pub fn double", 1, true) ~= nil, true)
    if os.getenv("SERPIS_TEST_SHOTS") then serpis.debug.screenshot(os.getenv("SERPIS_TEST_SHOTS") .. "/highlight.png") end
  end,
})
