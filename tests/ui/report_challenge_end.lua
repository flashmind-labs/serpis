-- A challenge always ends: its challenger ran out of output tokens while thinking (no report),
-- Stop on the report's header, its chat closed while it works (its worktree removed, say).
-- Each time the header says it stopped and offers Again.
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
local shots = os.getenv("SERPIS_TEST_SHOTS")
local function shot(name) if shots then d.screenshot(shots .. "/" .. name .. ".png", true) end end

local proj = os.getenv("HOME") .. "/proj"
os.execute("mkdir -p '" .. proj .. "' && cd '" .. proj .. "' && git init -q && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m start")

local function click(r)
  if not r then return end
  local x, y = r.x + r.w / 2, r.y + r.h / 2
  d.mouse_move(x, y)
  serpis.defer(100, function()
    d.mouse_button(x, y, "left", true)
    d.mouse_button(x, y, "left", false)
  end)
end

local chat
local function latest()
  local runs = serpis.chat.challenges(chat)
  return runs[#runs]
end
local function challenge(note)
  serpis.command.run("chat-report-challenge", { note = note })
end
-- The report's header in view, the transcript focused.
local function to_header()
  serpis.buffer.show(chat)
  serpis.command.run("chat-focus-transcript")
  serpis.command.run("beginning-of-buffer")
end
local function stopped(what, why)
  local r = latest()
  T.check(what .. ": it has ended", r and r.running, false)
  T.check(what .. ": why", r and r.error, why)
  T.check(what .. ": the header offers Again", d.rect("report-challenge-again") ~= nil, true)
  T.check(what .. ": and says so", d.rect("report-challenge-status") ~= nil, true)
  T.check(what .. ": no spinner", d.rect("report-challenge-spinner"), nil)
end

local steps = {
  function() serpis.command.run("new-chat", proj); return 600 end,
  function()
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "REPORT:skill")
    return 2500
  end,
  -- Out of output tokens while thinking: no report, but over.
  function()
    challenge("NO REPORT")
    return 3000
  end,
  function()
    to_header()
    return 600
  end,
  function()
    shot("1-no-report")
    stopped("no report", "it stopped without writing anything (out of output tokens, most likely)")
    T.check("no findings", serpis.chat.review(chat).drafts, 0)
    challenge("SLOW")
    return 2500
  end,
  function()
    to_header()
    return 600
  end,
  -- Stop, clicked.
  function()
    T.check("slow: it runs", latest() and latest().running, true)
    T.check("its header has a spinner", d.rect("report-challenge-spinner") ~= nil, true)
    click(d.rect("report-challenge-stop"))
    return 1500
  end,
  function()
    shot("2-stopped")
    stopped("Stop", "stopped")
    challenge("SLOW")
    return 2500
  end,
  -- Its chat closed while it works.
  function()
    local r = latest()
    T.check("again: it runs", r and r.running, true)
    T.check("its chat is open", r and r.challenger_buffer ~= nil, true)
    if r and r.challenger_buffer then serpis.buffer.kill(r.challenger_buffer) end
    return 600
  end,
  function()
    to_header()
    return 2000
  end,
  function()
    shot("3-closed")
    stopped("chat closed", "its chat was closed before it finished")
    T.finish()
  end,
}

local i = 0
local function go()
  i = i + 1
  local step = steps[i]
  if not step then return end
  local ok, wait = pcall(step)
  if not ok then
    T.check("step " .. i, wait, "no error")
    return T.finish()
  end
  serpis.defer(wait or 300, go)
end
go()
