-- Drives serpis through the promo's scenes while capturing its frames (frames/<ms>.png) and
-- scene marks (marks.txt: "<ms> <name>").
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local out = os.getenv("DEMO_OUT")
local t0
local function now() return math.floor((serpis.time() - t0) * 1000) end
local marks = io.open(out .. "/marks.txt", "w")
local function mark(name) marks:write(now(), " ", name, "\n"); marks:flush() end

serpis.var.set("remote-rayfish", false)
serpis.var.set("notify-backend", "none")
serpis.var.set("font-size", 16.5)
serpis.var.set("font-mono-size", 15.5)
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"openrouter":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":['
  .. '{"id":"anthropic/claude-sonnet-5.5","name":"Claude Sonnet 5.5","reasoning":true,"contextWindow":1000000,"cost":{"input":3,"output":15}}]}}}',
  os.getenv("DEMO_LLM_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("openrouter/anthropic/claude-sonnet-5.5")

-- Frames, as fast as the window gives them.
local recording = true
local function grab()
  if not recording then return end
  d.screenshot(string.format("%s/frames/%07d.png", out, now()), true)
  serpis.defer(40, grab)
end

local function when(ready, go, tries)
  tries = tries or 0
  if not ready() and tries < 300 then return serpis.defer(100, function() when(ready, go, tries + 1) end) end
  go()
end
-- Type like a person: a character at a time, a little uneven.
local function type_text(s, go, i)
  i = i or 1
  if i > #s then return serpis.defer(250, go) end
  d.text(s:sub(i, i))
  local c = s:sub(i, i)
  serpis.defer((c == " " and 70 or 38) + (i * 7919 % 23), function() type_text(s, go, i + 1) end)
end
local function key(k, label)
  marks:write(now(), " key ", label, "\n"); marks:flush()
  T.press(k)
end
local function wait(ms) return function(go) serpis.defer(ms, go) end end
local function idle() return not serpis.chat.running() end

local steps = {
  -- A chat on the demo model in place of the startup one (made before the model was set).
  function(go)
    local old = serpis.buffer.current()
    serpis.command.run("new-chat")
    serpis.buffer.kill(old)
    serpis.message("")
    serpis.defer(1200, go)
  end,
  function(go) mark("chat"); serpis.defer(500, go) end,
  function(go) type_text("The rate limiter test is flaky on CI. Can you find out why and fix it?", go) end,
  function(go) T.press("RET"); serpis.defer(800, function() when(idle, go) end) end,
  wait(2600),
  -- Windows: a terminal beside the chat, showing what the agent changed.
  function(go) mark("windows"); serpis.defer(400, go) end,
  function(go) key("s-\\", "⌘\\"); serpis.defer(500, go) end,
  function(go) key("s-t", "⌘T"); serpis.defer(1400, go) end,
  function(go) type_text("git diff --stat && git diff | head -20", go) end,
  function(go) T.press("RET"); serpis.defer(1600, go) end,
  function(go) key("s-|", "⌘|"); serpis.defer(400, go) end,
  function(go) key("s-n", "⌘N"); serpis.defer(1600, go) end,
  -- The palette.
  function(go) mark("palette"); serpis.defer(300, go) end,
  function(go) key("s-P", "⌘⇧P"); serpis.defer(600, go) end,
  function(go) type_text("split", go) end,
  wait(1300),
  function(go) T.press("ESC"); serpis.defer(50, function() serpis.message(""); serpis.defer(550, go) end) end,
  -- The agent reshapes serpis itself.
  function(go) mark("extend"); serpis.defer(300, go) end,
  function(go) type_text("Make the font a bit bigger", go) end,
  function(go) T.press("RET"); serpis.defer(800, function() when(idle, go) end) end,
  wait(3000),
  function(go) mark("end"); recording = false; serpis.defer(1500, go) end,
  function() marks:close(); d.quit() end,
}

local i = 0
local function go()
  i = i + 1
  if steps[i] then steps[i](go) end
end
serpis.defer(1800, function()
  d.resize(1440, 860)
  t0 = serpis.time()
  serpis.defer(1200, function()
    grab()
    go()
  end)
end)
