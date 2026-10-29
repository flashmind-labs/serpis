-- The chat composer's controls, clicked with the mouse. Runs a fake provider that refuses
-- connections, so nothing reaches a real model.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write('{"providers":{"fake":{"baseUrl":"http://127.0.0.1:9/v1","api":"openai-completions","apiKey":"x","models":[{"id":"m","reasoning":true}]}}}')
f:close()
serpis.agent.reload_models()
serpis.agent.model("fake/m")
serpis.var.set("session-directories", {})
serpis.var.set("session-save", false)

local function steps(list)
  local i = 0
  local function go()
    i = i + 1
    if not list[i] then return T.finish() end
    local wait = list[i](go)
    if wait ~= false then serpis.defer(wait or 400, go) end
  end
  go()
end
local function click(name)
  local r = d.composer()[name]
  if not r then return false end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  d.mouse_button(x, y, "left", true)
  d.mouse_button(x, y, "left", false)
  return true
end
local function state() return serpis.buffer.activity().state end

local chat
steps({
  function()
    serpis.command.run("new-chat")
    chat = serpis.buffer.current()
    return 600
  end,
  function()
    local hits = d.composer()
    T.check("controls drawn", hits.model ~= nil and hits.thinking ~= nil and hits.send ~= nil, true)
    T.check("model pill click", click("model"), true)
  end,
  function()
    T.check("model pill opens the model picker", (d.minibuffer() or ""):find("[Mm]odel") ~= nil, true)
    T.press("ESC")
  end,
  function()
    T.check("picker closed", d.minibuffer(), nil)
    click("thinking")
  end,
  function()
    T.check("thinking pill opens the thinking picker", (d.minibuffer() or ""):find("Thinking") ~= nil, true)
    T.press("ESC")
  end,
  function()
    -- An empty prompt doesn't send.
    click("send")
  end,
  function()
    T.check("empty send does nothing", state(), "idle")
    T.check("still in the chat", serpis.buffer.current(), chat)
    d.text("hello")
  end,
  function()
    click("send")
    return 300
  end,
  function()
    T.check("send starts the agent", state(), "working")
    -- What it has spent shows while it works, not only once it stops.
    -- (With no room for both, the activity gives way: the usage stays.)
    T.check("usage shows while it works", d.composer().usage ~= nil, true)
    T.check("prompt sent", (serpis.buffer.text() or ""):find("hello") ~= nil, true)
    -- The same button stops it.
    click("send")
    return 800
  end,
  function()
    T.check("stop aborts the agent", state() ~= "working", true)
    -- (However wide the window manager made the window.)
    if d.composer().card.w >= 520 then
      T.check("all its pills shown when there's room", d.composer()["keep-going"] ~= nil and d.composer().machine ~= nil, true)
    end
    -- A narrow window at a large font: what doesn't fit is left out, never under the send button.
    serpis.var.set("font-size", 20)
    T.press("s-\\")
  end,
  function()
    local hits = d.composer()
    local clear = true
    for _, name in ipairs({ "model", "thinking", "machine", "keep-going" }) do
      local r = hits[name]
      if r and r.x + r.w > hits.send.x then clear = false end
    end
    T.check("pills stay clear of the send button", clear, true)
    T.check("keep-going left out first", hits["keep-going"], nil)
    -- (A narrow window, split, can leave room for none of them.)
    if hits.card.w >= 200 then T.check("the model shows while there's room for it", hits.model ~= nil, true) end
    serpis.var.set("font-size", 14.5)
  end,
})
