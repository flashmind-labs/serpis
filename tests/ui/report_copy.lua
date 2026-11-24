-- A report's header has a copy icon: clicked, the report goes to the clipboard as the markdown
-- it was written in. chat-report-copy, the command, does the same; the variable off, no icon.
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

-- What the mock agent writes for REPORT:skill.
local rf = io.open(os.getenv("SERPIS_TEST_DIR") .. "/report-skill.md")
local report = rf:read("a")
rf:close()
local function trim(s) return (s or ""):gsub("^%s+", ""):gsub("%s+$", "") end

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
local steps = {
  function() serpis.command.run("new-chat"); return 600 end,
  function()
    chat = serpis.buffer.current()
    serpis.chat.prompt(chat, "REPORT:skill")
    return 2500
  end,
  function()
    serpis.command.run("chat-focus-transcript")
    serpis.command.run("beginning-of-buffer")
    return 600
  end,
  function()
    shot("1-icon")
    T.check("the report's header has a copy icon", d.rect("report-copy") ~= nil, true)
    T.check("nothing copied yet", d.copied(), nil)
    click(d.rect("report-copy"))
    return 800
  end,
  function()
    shot("2-copied")
    T.check("clicked: the report is copied as its markdown", trim(d.copied()), trim(report))
    T.check("with the markdown kept", (d.copied() or ""):find("## ", 1, true) ~= nil, true)
    return 300
  end,
  function()
    -- The command.
    serpis.command.run("chat-report-copy")
    T.check("chat-report-copy copies the latest report", trim(d.copied()), trim(report))
    serpis.var.set("chat-report-copy", false)
    return 600
  end,
  function()
    T.check("off: no copy icon", d.rect("report-copy"), nil)
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
