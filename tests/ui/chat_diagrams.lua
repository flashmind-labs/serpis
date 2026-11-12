-- Mermaid diagrams in a chat are drawn, as part of the page (chat-diagrams): a flowchart, a
-- sequence and a state diagram, each fitting the column; one that can't be drawn stays as code,
-- saying so; with the setting off, all of them are code. SERPIS_TEST_SHOTS: screenshots of
-- them in a dark and a light theme, and with a node under the mouse.
local T = dofile(os.getenv("SERPIS_TEST_DIR") .. "/lib.lua")
local d = serpis.debug
local agent_dir = os.getenv("SERPIS_AGENT_DIR")
os.execute("mkdir -p '" .. agent_dir .. "'")
local f = io.open(agent_dir .. "/models.json", "w")
f:write(string.format('{"providers":{"mock":{"baseUrl":"%s","api":"openai-completions","apiKey":"x","models":[{"id":"m"}]}}}', os.getenv("SERPIS_TEST_MOCK_URL")))
f:close()
serpis.agent.reload_models()
serpis.agent.model("mock/m")
serpis.var.set("remote-rayfish", false)
serpis.var.set("sidebar-visible", false)
serpis.var.set("scroll-animation-time", 0)
local shots = os.getenv("SERPIS_TEST_SHOTS")
local function last_request()
  local last
  local fh = io.open(os.getenv("SERPIS_TEST_MOCK_LOG"))
  if fh then
    for l in fh:lines() do last = l end
    fh:close()
  end
  return last or ""
end

local function diagrams()
  local out = {}
  for _, l in ipairs(d.chat_lines() or {}) do
    if l.diagram then out[#out + 1] = l end
  end
  return out
end
local function has(list, x)
  for _, v in ipairs(list) do if v == x then return true end end
  return false
end
local function shot(name)
  if shots then d.screenshot(shots .. "/" .. name .. ".png", true) end
end
-- Scroll the transcript so the line with `text` is at the top: from the end, the cursor moved
-- up to it.
local function show(text)
  serpis.command.run("chat-focus-transcript")
  local lines = d.chat_lines() or {}
  serpis.buffer.set_cursor(#lines, 1)
  if text == "can't draw" then return end
  for i, l in ipairs(lines) do
    if l.text:find(text, 1, true) then
      serpis.buffer.set_cursor(i, 1)
      return
    end
  end
end

local steps = {
  function() serpis.command.run("new-chat"); return 600 end,
  function() serpis.chat.prompt(serpis.buffer.current(), "REPORT:diagram"); return 2500 end,
  function()
    T.check("the agent is offered the diagrams skill", last_request():find('"diagrams_skill": true', 1, true) ~= nil, true)
    local ds = diagrams()
    T.check("three diagrams drawn", #ds, 3)
    local flow, seq, state = ds[1], ds[2], ds[3]
    T.check("a flowchart", flow and flow.diagram.kind, "flowchart")
    T.check("its nodes", flow and has(flow.diagram.nodes, "Chat") and has(flow.diagram.nodes, "Loop") and has(flow.diagram.nodes, "Review"), true)
    T.check("a sequence diagram", seq and seq.diagram.kind:find("sequence") ~= nil, true)
    T.check("a state diagram", state and state.diagram.kind:find("state") ~= nil, true)
    for _, l in ipairs(ds) do
      T.check(l.diagram.kind .. " fits the column", l.diagram.width <= l.column + 0.5, true)
    end
    local code = false
    for _, l in ipairs(d.chat_lines() or {}) do
      if l.text:find("mermaid · not drawn", 1, true) then code = true end
    end
    T.check("one that can't be drawn is code, saying so", code, true)
    for _, l in ipairs(ds) do
      T.check(l.diagram.kind .. " centered", math.abs(l.indent * 2 + l.diagram.width - l.column) < 2 or l.diagram.width >= l.column - 1, true)
    end
    -- Selected and copied, a diagram is its source.
    local at
    for i, l in ipairs(d.chat_lines() or {}) do
      if l.diagram and not at then at = i end
    end
    serpis.command.run("chat-focus-transcript")
    serpis.buffer.set_cursor(at, 1)
    serpis.command.run("set-mark-command")
    serpis.buffer.set_cursor(at + 1, 1)
    local region = serpis.buffer.region() or ""
    T.check("copied, it is its source", region:find("```mermaid\nflowchart LR\n", 1, true) ~= nil, true)
    serpis.command.run("keyboard-quit")
    serpis.command.run("select-theme", "serpis-dark")
    return 1200
  end,
  function() show("The pieces"); return 900 end,
  function() shot("dark-flow"); return 600 end,
  function()
    -- Over a node: it and its lines lit.
    d.mouse_move(tonumber(os.getenv("SERPIS_HOVER_X") or "330"), tonumber(os.getenv("SERPIS_HOVER_Y") or "190"))
    return 600
  end,
  function() shot("dark-hover"); return 600 end,
  function() show("What happens when"); return 900 end,
  function() shot("dark-sequence"); return 600 end,
  function() show("can't draw"); return 900 end,
  function() shot("dark-state"); return 600 end,
  function() serpis.command.run("select-theme", "serpis-light"); return 1200 end,
  function() show("The pieces"); return 900 end,
  function() shot("light-flow"); return 600 end,
  function() show("What happens when"); return 900 end,
  function() shot("light-sequence"); return 600 end,
  function() show("can't draw"); return 900 end,
  function() shot("light-state"); return 600 end,
  function()
    serpis.var.set("chat-diagrams", false)
    return 800
  end,
  function()
    T.check("off: none drawn", #diagrams(), 0)
    local fences = 0
    for _, l in ipairs(d.chat_lines() or {}) do
      if l.text == "mermaid" then fences = fences + 1 end
    end
    T.check("all of them code", fences, 4)
    -- And a chat started with it off isn't offered the skill.
    serpis.command.run("new-chat")
    return 600
  end,
  function() serpis.chat.prompt(serpis.buffer.current(), "hello"); return 2000 end,
  function()
    T.check("off: no diagrams skill", last_request():find('"diagrams_skill": false', 1, true) ~= nil, true)
    T.finish()
    return 100000
  end,
}
local i = 0
local function go()
  i = i + 1
  if not steps[i] then return end
  local ok, wait = pcall(steps[i])
  if not ok then
    T.check("step " .. i, wait, "no error")
    return T.finish()
  end
  serpis.defer(wait or 300, go)
end
go()
