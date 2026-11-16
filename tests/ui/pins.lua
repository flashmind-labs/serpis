-- Persistent chat shortcuts in the sidebar and pinned-chats dashboard. pins_restore.lua runs
-- next with the same state and verifies a closed pin can resume after restarting serpis.
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
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
sh("mkdir -p '" .. agent_dir .. "'")
local models = io.open(agent_dir .. "/models.json", "w")
models:write('{"providers":{"fake":{"baseUrl":"http://127.0.0.1:9/v1","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}')
models:close()
serpis.agent.reload_models()
serpis.agent.model("fake/m")
serpis.var.set("session-directories", {})

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    local step = list[i]
    if not step then return T.finish() end
    if step(go) ~= false then serpis.defer(300, go) end
  end
  go()
end

local function settled(id, done, n)
  n = n or 0
  if serpis.buffer.activity(id).state == "working" and n < 100 then
    return serpis.defer(200, function() settled(id, done, n + 1) end)
  end
  done()
end

local function pin_rows()
  local out = {}
  for _, row in ipairs(d.sidebar_rows()) do
    if row.kind == "pin" then table.insert(out, row) end
  end
  return out
end

local first, second, first_id
steps({
  function()
    serpis.command.run("new-chat", proj)
    first = serpis.buffer.current()
    serpis.chat.prompt(first, "first priority")
    return 500
  end,
  function(next)
    settled(first, next)
    return false
  end,
  function()
    first_id = serpis.chat.info(first).session_id
    T.check("first session has stable id", first_id ~= nil and first_id ~= "", true)
    T.check("pin API adds first", serpis.chat.pin(first), true)
    T.check("pin API is idempotent", serpis.chat.pin(first), false)
    serpis.command.run("new-chat", proj)
    second = serpis.buffer.current()
    serpis.chat.prompt(second, "second priority")
    return 500
  end,
  function(next)
    settled(second, next)
    return false
  end,
  function()
    serpis.command.run("chat-pin", second)
    return 400
  end,
  function()
    local pins = serpis.chat.pinned()
    T.check("two ordered pins", #pins, 2)
    T.check("first keeps stable identity", pins[1].id, first_id)
    T.check("open pin reports buffer", pins[1].buffer, first)
    T.check("sidebar has two pin rows", #pin_rows(), 2)
    if shots then d.screenshot(shots .. "/pins-sidebar.png") end

    local windows = serpis.window.count()
    serpis.command.run("pinned-chats-split")
    T.check("dashboard opens in split", serpis.window.count(), windows + 1)
    T.check("dashboard buffer kind", serpis.buffer.kind(), "pinned-chats")
    return 400
  end,
  function()
    return 400
  end,
  function()
    if shots then d.screenshot(shots .. "/pins-dashboard.png") end
    return 400
  end,
  function()
    serpis.command.run("chat-pin-open", 1)
    return 400
  end,
  function()
    T.check("numbered slot opens first", serpis.buffer.current(), first)
    serpis.buffer.kill(first)
    serpis.command.run("session-rescan")
    return 900
  end,
  function()
    local pins = serpis.chat.pinned()
    T.check("closed pin stays listed", #pins, 2)
    T.check("closed pin has no buffer", pins[1].buffer, nil)
    T.check("closed saved pin is available", pins[1].available, true)
    T.check("sidebar keeps closed pin", #pin_rows(), 2)
    local file = io.open(os.getenv("SERPIS_TEST_OUT") .. ".pin", "w")
    file:write(first_id)
    file:close()
  end,
})
