-- Tables in a report (in its opening, in Needs you, in Changed) keep their own size and the
-- width they were laid out for: a row doesn't wrap at chat-measure, nor grow with
-- chat-report-scale, as prose does; Needs you's card makes room for them; their edges show on
-- the report's surface. A plain message's tables don't wrap at the measure either.
-- SERPIS_TEST_SHOTS: the report in a dark and a light theme, and as a plain message.
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
local function shot(name)
  if shots then d.screenshot(shots .. "/" .. name .. ".png", true) end
end
local function line(text)
  for _, l in ipairs(d.chat_lines() or {}) do
    if l.text:find(text, 1, true) then return l end
  end
end
local function show(text)
  serpis.command.run("chat-focus-transcript")
  local lines = d.chat_lines() or {}
  serpis.buffer.set_cursor(#lines, 1)
  for i, l in ipairs(lines) do
    if l.text:find(text, 1, true) then serpis.buffer.set_cursor(i, 1); return end
  end
end
-- A table's row: its own size, no measure.
local function kept(l) return l ~= nil and l.scale == nil and l.measure == nil end

local plain_border
local steps = {
  function() serpis.command.run("select-theme", "serpis-dark"); serpis.command.run("new-chat"); return 600 end,
  function() serpis.chat.prompt(serpis.buffer.current(), "REPORT:table"); return 2500 end,
  function()
    T.check("a report", line("Report  ·  m") ~= nil, true)
    local prose = line("Eviction by size")
    T.check("its prose takes the report's size and measure", prose ~= nil and prose.scale ~= nil and prose.measure ~= nil, true)
    T.check("a table in its opening keeps its own", kept(line("Long session with many tool calls")), true)
    T.check("one in Needs you too", kept(line("256 MB 78%")), true)
    T.check("and one in Changed", kept(line("eviction by size, measured on insert")), true)
    local wide = line("eviction by size, measured on insert")
    local cap = line("256 MB 78%")
    T.check("Needs you's table sits in its card", cap ~= nil and cap.bar ~= nil, true)
    T.check("its edges set against the surface", wide ~= nil and wide.border ~= nil, true)
    plain_border = wide and wide.border
    show("Done")
    return 600
  end,
  function() shot("dark-report"); return 600 end,
  function() serpis.command.run("select-theme", "serpis-light"); return 1200 end,
  function() show("Done"); return 600 end,
  function() shot("light-report"); return 600 end,
  function()
    serpis.command.run("select-theme", "serpis-dark")
    serpis.var.set("chat-report", false)
    return 1200
  end,
  function()
    T.check("as a plain message, its tables don't wrap at the measure", kept(line("Long session with many tool calls")), true)
    local wide = line("eviction by size, measured on insert")
    T.check("their edges the theme's", wide ~= nil and wide.border ~= nil and wide.border ~= plain_border, true)
    show("Done")
    return 600
  end,
  function() shot("dark-plain"); T.finish(); return 100000 end,
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
